use std::path::PathBuf;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use tungstenite::http::Uri;
use tungstenite::{ClientRequestBuilder, connect};

use log::{debug, error, info};

#[derive(PartialEq, Clone, Debug)]
enum Auth {
    Token(String),
    Basic(String, Option<String>),
    None,
}

impl Auth {
    fn to_header(&self) -> Option<String> {
        match self {
            Auth::Token(token) => Some(format!("Bearer {}", token)),
            Auth::Basic(username, password) => {
                let val = BASE64_STANDARD.encode(format!(
                    "{}:{}",
                    username,
                    match password {
                        Some(password) => password,
                        None => "",
                    }
                ));
                Some(format!("Basic {}", val))
            }
            Auth::None => None,
        }
    }
}

#[derive(PartialEq, Debug)]
struct Filters {
    message: Option<String>,
    title: Option<String>,
    priority: Option<i64>,
    tags: Option<Vec<String>>,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            message: None,
            title: None,
            priority: None,
            tags: None,
        }
    }
}

impl Filters {
    fn generate_query_params(&self) -> String {
        let mut query = vec![];
        if let Some(message) = &self.message {
            query.push(format!("message={}", message));
        }
        if let Some(title) = &self.title {
            query.push(format!("title={}", title));
        }
        if let Some(priority) = self.priority {
            query.push(format!("priority={}", priority));
        }
        if let Some(tags) = &self.tags {
            query.push(format!("tags={}", tags.join(",")));
        }
        query.join("&")
    }
}

struct ServerConnection {
    server: String,
    auth: Auth,
    filters: Option<Filters>,
    topics: Vec<String>,
    topic_script_map: std::collections::HashMap<String, PathBuf>,
    retry_count: u8,
}

impl ServerConnection {
    fn connect(mut self) {
        let topic = self.topics.join(",");
        let url = self.wss_url(topic.as_str());

        info!("Connecting to {}", url);
        debug!(
            "Authorization: {}",
            self.auth.to_header().unwrap_or("None".to_string())
        );

        let client = match self.auth.to_header() {
            Some(auth) => ClientRequestBuilder::new(url).with_header("Authorization", auth),
            None => ClientRequestBuilder::new(url),
        };

        let (mut socket, response) = match connect(client) {
            Ok((socket, response)) => (socket, response),
            Err(err) => {
                error!("Failed to connect to server: {}", err);
                error!("Server: {}", self.server);
                error!("Topic: {}", topic);
                error!("Authorization: {:?}", self.auth);
                error!("Filters: {:?}", self.filters);

                match err {
                    tungstenite::Error::Http(response) => {
                        error!("Response HTTP code: {}", response.status());
                        error!("Response HTTP headers: {:#?}", response.headers());
                        error!("Response HTTP body: {:?}", response.body());
                    }
                    _ => {
                        debug!("Unhandled error: {:?}", err);
                    }
                }
                // retry connection if error 3 times (race condition in v.User() on the server side
                // sometimes causes an error which is caught as a generic error which the server
                // determines to be "unauthenticated" and closes the connection)
                // github issue: https://github.com/binwiederhier/ntfy/issues/1051
                // faulty code: https://github.com/binwiederhier/ntfy/blob/630f2957deb670dcacfe0a338091d7561f176b9c/server/server.go#L1905-L1910
                // this is a workaround for now, but should be fixed in the server code

                if self.retry_count < 3 {
                    self.retry_count += 1;
                    info!("Retrying: {}", self.retry_count);
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    self.connect();
                } else {
                    error!("Failed to connect to server after 3 attempts");
                }

                return;
            }
        };

        info!("Connected to server");
        debug!("Response HTTP code: {}", response.status());
        debug!("Response HTTP headers: {:#?}", response.headers());

        loop {
            let msg = socket.read().expect("failed to read message");
            match msg {
                tungstenite::Message::Text(text) => {
                    // json load the message
                    let msg: serde_json::Value = serde_json::from_str(&text)
                        .expect(format!("failed to parse message json: {}", text).as_str());

                    // check if the message is an open event
                    if msg["event"].as_str().unwrap_or("") == "open" {
                        // check if the topic is the same as the one we subscribed to
                        if msg["topic"].as_str().unwrap_or("") == topic {
                            debug!("Connected to correct topic");
                        } else {
                            todo!("handle open event for different topic then requested");
                        }
                    }

                    // check if message is a message event
                    // if it is, parse the message for the topic
                    // if the topic has a script, execute the script with the message
                    // otherwise check if there is a default script to execute
                    // if there is no default script, print the message

                    if msg["event"].as_str().unwrap_or("") == "message" {
                        let topic = msg["topic"].as_str().unwrap_or("");
                        if let Some(script) = self.topic_script_map.get(topic) {
                            let script = script.clone();
                            std::thread::spawn(move || {
                                execute_script(&script, &msg);
                            });
                        } else {
                            // current version of ntfy returns a newline at the end of the message
                            // so we don't need to add a newline
                            print!("{}", msg["message"].as_str().unwrap_or(""));
                        }
                    }
                }
                tungstenite::Message::Close(close) => {
                    println!("Received close message: {:?}", close);
                    break;
                }
                _ => {
                    debug!("Received unhandled message: {:?}", msg);
                }
            }
        }
    }

    fn wss_url(&self, topic: &str) -> Uri {
        let mut server = self
            .server
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        if server == self.server {
            // assume http if no protocol is given
            server = format!("ws://{}", server);
        }
        let query_params;
        if let Some(filters) = &self.filters {
            let query = filters.generate_query_params();
            if query != "" {
                query_params = format!("?{}", query);
            } else {
                query_params = "".to_string();
            }
        } else {
            query_params = "".to_string();
        }

        format!("{}/{}/ws{}", server, topic, query_params)
            .parse()
            .expect("failed to parse url, make sure default-host is a valid url")
    }
}

struct NtfyClient {
    default_server: String,
    default_auth: Auth,
    default_command: Option<String>,
    subscriptions: Vec<ServerConnection>,
}

impl NtfyClient {
    // function to add a new topic to the client
    fn add_topic(self: &mut NtfyClient, yml_data: serde_yml::Value) {
        // check for each of the following: user, password, token, command, if

        // extract server and topic from yml_data
        // check if "topic" belongs to a server (has a / in it)
        // if it does, split the topic into server and topic
        // otherwise, add the topic to the default server
        let topic_str = yml_data["topic"]
            .as_str()
            .expect("topic should be a string");
        let topic = topic_str.split("/").collect::<Vec<&str>>();
        let (server, topic) = match topic.len() {
            1 => (None, topic_str),
            2 => (Some(topic[0]), topic[1]),
            _ => panic!("topic should not contain more than one /"),
        };

        let server = match server {
            Some(server) => server.to_string(),
            None => self.default_server.clone(),
        };

        // map user, password, token to the Auth struct
        let token: Option<&str> = yml_data["token"].as_str();
        let user: Option<&str> = yml_data["user"].as_str();
        let password: Option<&str> = yml_data["password"].as_str();

        // prioritize token over user and password
        let auth = match token {
            Some(token) => Auth::Token(token.to_string()),
            None => match (user, password) {
                (Some(user), Some(password)) => {
                    Auth::Basic(user.to_string(), Some(password.to_string()))
                }
                (Some(user), None) => Auth::Basic(user.to_string(), None),
                _ => self.default_auth.clone(),
            },
        };

        let mut filters: Option<Filters> = None;

        if let serde_yml::Value::Mapping(filter_raw) = &yml_data["if"] {
            // map if to the Filter struct
            let message: Option<&str> = match filter_raw.get("message") {
                Some(message) => Some(message.as_str().unwrap_or("")),
                _ => None,
            };
            let title: Option<&str> = match filter_raw.get("title") {
                Some(title) => Some(title.as_str().unwrap_or("")),
                _ => None,
            };
            let priority: Option<i64> = match filter_raw.get("priority") {
                Some(priority) => Some(priority.as_i64().unwrap_or(3)),
                None => None,
            };
            let tags: Option<Vec<&str>> = match filter_raw.get("tags") {
                Some(tags) => {
                    let tags = match tags {
                        serde_yml::Value::String(tags) => tags.as_str(),
                        _ => "",
                    };
                    Some(
                        tags.split(",")
                            .collect::<Vec<&str>>()
                            .iter()
                            .map(|x| x.trim())
                            .collect(),
                    )
                }
                None => None,
            };

            let local_filters = Filters {
                message: message.map(|x| x.to_string()),
                title: title.map(|x| x.to_string()),
                priority,
                tags: tags.map(|x| x.iter().map(|x| x.to_string()).collect()),
            };

            // check if all fields are empty
            if local_filters != Filters::default() {
                filters = Some(local_filters);
            }
        }

        let command = yml_data["command"].as_str();

        let command: Option<PathBuf> = match command {
            Some(command) => Some(PathBuf::from(command)),
            None => match &self.default_command {
                Some(command) => Some(PathBuf::from(command)),
                None => None,
            },
        };

        // loop over subscriptions and compare auth, server, and filters
        // if a subscription with the same auth, server, and filters exists, add the topic to the
        // subscription
        // otherwise, create a new subscription with the topic

        let subscriptions = &mut self.subscriptions;

        for sub in subscriptions.iter_mut() {
            if sub.auth == auth && sub.server == server && sub.filters == filters {
                sub.topics.push(topic.to_string());
                return;
            }
        }
        // create a new subscription
        let new_sub = ServerConnection {
            server,
            auth,
            filters,
            topics: vec![topic.to_string()],
            topic_script_map: match command {
                Some(command) => {
                    let mut map = std::collections::HashMap::new();
                    map.insert(topic.to_string(), PathBuf::from(command));
                    map
                }
                None => std::collections::HashMap::new(),
            },
            retry_count: 0,
        };
        self.subscriptions.push(new_sub);
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // load ntfy config file
    let xdg_dir = xdg::BaseDirectories::with_prefix("ntfy").expect("failed to get ntfy config dir");
    let config_path = xdg_dir
        .find_config_file("client.yml")
        .expect("failed to find ntfy config file");
    let file = std::fs::File::open(config_path).expect("failed to open ntfy config file");
    let config: serde_yml::Value =
        serde_yml::from_reader(file).expect("failed to parse ntfy config file");

    // get server address from config
    let server = config["default-host"].as_str().unwrap_or("https://ntfy.sh");

    // get credentials from config
    // try token first, then username and password
    let mut auth: Option<Auth> = None;
    if let Some(token) = config["default-token"].as_str() {
        // use token
        auth = Some(Auth::Token(token.to_string()));
    }
    if auth.is_none() && config["default-username"].is_string() {
        let username = config["username"]
            .as_str()
            .expect("const object changed in memory");
        let password = match config["password"].as_str() {
            Some(password) => Some(password.to_string()),
            None => None,
        };
        auth = Some(Auth::Basic(username.to_string(), password));
    }
    let auth = auth.unwrap_or(Auth::None);

    // check if default-command is set
    let default_command_path: Option<&str> = config["default-command"].as_str();

    // create a new NtfyClient
    let mut client = NtfyClient {
        default_server: server.to_string(),
        default_auth: auth,
        default_command: default_command_path.map(|x| x.to_string()),
        subscriptions: vec![],
    };

    // loop over topics in config
    // add each topic to the client
    if let serde_yml::Value::Sequence(topics) = &config["subscribe"] {
        for topic in topics.iter() {
            client.add_topic(topic.clone());
        }
    }

    // log number of subscriptions
    info!("Number of subscriptions: {}", client.subscriptions.len());

    // loop over subscriptions in client
    // connect to each subscription
    // handle messages
    let mut threads = vec![];
    for x in client.subscriptions {
        threads.push(std::thread::spawn(move || {
            x.connect();
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
}

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// !WARNING this function executes scripts with the same permissions as the ntfy client
/// make sure the client is running with the least amount of permissions needed
fn execute_script(path: &PathBuf, msg: &serde_json::Value) {
    // check if script exists
    if !path.exists() {
        error!("Script does not exist: {}", path.display());
        return;
    }

    // check if script is executable
    if !path.metadata().unwrap().permissions().mode() & 0o111 != 0 {
        error!("Script is not executable: {}", path.display());
        return;
    }

    // set env vars for script cloning the following go code using the body of the message:
    // env := make([]string, 0)
    // env = append(env, envVar(m.ID, "NTFY_ID", "id")...)
    // env = append(env, envVar(m.Topic, "NTFY_TOPIC", "topic")...)
    // env = append(env, envVar(fmt.Sprintf("%d", m.Time), "NTFY_TIME", "time")...)
    // env = append(env, envVar(m.Message, "NTFY_MESSAGE", "message", "m")...)
    // env = append(env, envVar(m.Title, "NTFY_TITLE", "title", "t")...)
    // env = append(env, envVar(fmt.Sprintf("%d", m.Priority), "NTFY_PRIORITY", "priority", "prio", "p")...)
    // env = append(env, envVar(strings.Join(m.Tags, ","), "NTFY_TAGS", "tags", "tag", "ta")...)
    // env = append(env, envVar(m.Raw, "NTFY_RAW", "raw")...)

    // initialize non string values outside to prevent temporary allocation
    // errors

    let time = msg["time"].as_i64().unwrap_or(0).to_string();
    let priority = msg["priority"].as_i64().unwrap_or(3).to_string();
    let arr = vec![];
    let tags = msg["tags"]
        .as_array()
        .unwrap_or(&arr)
        .into_iter()
        .map(|x| x.as_str().unwrap_or(""))
        .collect::<Vec<&str>>();

    let test = tags.join(",");
    let raw_json = msg.to_string();

    let env_vars = vec![
        ("NTFY_ID", msg["id"].as_str().unwrap_or("")),
        ("NTFY_TOPIC", msg["topic"].as_str().unwrap_or("")),
        ("NTFY_TIME", time.as_str()),
        ("NTFY_MESSAGE", msg["message"].as_str().unwrap_or("")),
        ("NTFY_TITLE", msg["title"].as_str().unwrap_or("")),
        ("NTFY_PRIORITY", priority.as_str()),
        ("NTFY_TAGS", test.as_str()),
        ("NTFY_RAW", raw_json.as_str()),
    ];

    debug!("{:?}", &env_vars);

    // execute script with env vars
    let _output = Command::new(path)
        .envs(env_vars)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to execute script");
}
