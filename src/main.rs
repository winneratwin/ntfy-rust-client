use std::path::PathBuf;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use tungstenite::http::Uri;
use tungstenite::{ClientRequestBuilder, connect};

use log::{debug, info};

fn main() {
    env_logger::init();

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
    let mut auth_header: Option<String> = None;
    if let Some(token) = config["default-token"].as_str() {
        // use token
        auth_header = Some(format!("Bearer {}", token));
    }
    if auth_header.is_none()
        && config["default-username"].is_string()
        && config["default-password"].is_string()
    {
        let username = config["username"]
            .as_str()
            .expect("const object changed in memory");
        let password = config["password"]
            .as_str()
            .expect("const object changed in memory");
        // auth header is
        // "Basic " + base64(username + ":" + password) // -> Basic dGVzdHVzZXI6ZmFrZXBhc3N3b3Jk
        let val = BASE64_STANDARD.encode(format!("{}:{}", username, password));
        auth_header = Some(format!("Basic {}", val));
    }

    // check if default-command is set
    let default_command_path: Option<&str> = config["default-command"].as_str();

    // get all config["subscribe"]["topic"] as Vec<&str>
    let subscriptions: Vec<&str> = config["subscribe"]
        .as_sequence()
        .expect("subscribe should be a list")
        .iter()
        .map(|x| x["topic"].as_str().expect("topic should be a string"))
        .collect();

    // create a map of topic -> script for non-default scripts
    let mut topic_script_map: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();
    for sub in config["subscribe"]
        .as_sequence()
        .expect("subscribe should be a list")
    {
        if let Some(script) = sub["script"].as_str() {
            topic_script_map.insert(
                sub["topic"]
                    .as_str()
                    .expect("topic should be a string")
                    .to_string(),
                PathBuf::from(script),
            );
        }
    }

    // combine to default-host/{subscriptions combined with commas}/ws
    // e.g. https://ntfy.sh/topic1,topic2,topic3/ws

    // change https to wss
    // change http to ws
    let server = server
        .replace("http://", "ws://")
        .replace("https://", "wss://");

    let topic = subscriptions.join(",");
    let url: Uri = format!("{}/{}/ws", server, topic)
        .parse()
        .expect("failed to parse url, make sure default-host is a valid url");
    info!("Connecting to {}", url);
    debug!(
        "Authorization: {}",
        auth_header.clone().or(Some("None".to_string())).unwrap()
    );

    let client = match auth_header {
        Some(_) => {
            ClientRequestBuilder::new(url).with_header("Authorization", auth_header.unwrap())
        }
        None => ClientRequestBuilder::new(url),
    };

    let (mut socket, _response) = connect(client).expect("failed to connect to server");

    info!("Connected to server");
    // println!("Response HTTP code: {}", response.status());
    // println!("Response HTTP headers: {:#?}", response.headers());

    loop {
        let msg = socket.read().expect("failed to read message");
        match msg {
            tungstenite::Message::Text(text) => {
                // // List of possible events
                // const (
                // 	openEvent        = "open"
                // 	keepaliveEvent   = "keepalive"
                // 	messageEvent     = "message"
                // 	pollRequestEvent = "poll_request"
                // )

                // // message represents a message published to a topic
                // type message struct {
                // 	ID          string      `json:"id"`                // Random message ID
                // 	Time        int64       `json:"time"`              // Unix time in seconds
                // 	Expires     int64       `json:"expires,omitempty"` // Unix time in seconds (not required for open/keepalive)
                // 	Event       string      `json:"event"`             // One of the above
                // 	Topic       string      `json:"topic"`
                // 	Title       string      `json:"title,omitempty"`
                // 	Message     string      `json:"message,omitempty"`
                // 	Priority    int         `json:"priority,omitempty"`
                // 	Tags        []string    `json:"tags,omitempty"`
                // 	Click       string      `json:"click,omitempty"`
                // 	Icon        string      `json:"icon,omitempty"`
                // 	Actions     []*action   `json:"actions,omitempty"`
                // 	Attachment  *attachment `json:"attachment,omitempty"`
                // 	PollID      string      `json:"poll_id,omitempty"`
                // 	ContentType string      `json:"content_type,omitempty"` // text/plain by default (if empty), or text/markdown
                // 	Encoding    string      `json:"encoding,omitempty"`     // empty for raw UTF-8, or "base64" for encoded bytes
                // 	Sender      netip.Addr  `json:"-"`                      // IP address of uploader, used for rate limiting
                // 	User        string      `json:"-"`                      // UserID of the uploader, used to associated attachments
                // }

                // open events are sent when a client connects to a topic
                // double check if the topic is the same as the one we subscribed to

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
                    if let Some(script) = topic_script_map.get(topic) {
                        let script = script.clone();
                        std::thread::spawn(move || {
                            execute_script(&script, &msg);
                        });
                    } else if let Some(script) = default_command_path {
                        let script = script.to_string();
                        std::thread::spawn(move || {
                            execute_script(&PathBuf::from(script), &msg);
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
                //println!("Received unhandled message");
            }
        }
    }
}

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// !WARNING this function executes scripts with the same permissions as the ntfy client
/// make sure the client is running with the least amount of permissions needed
fn execute_script(path: &PathBuf, msg: &serde_json::Value) {
    // check if script exists
    if !path.exists() {
        eprintln!("Script does not exist: {}", path.display());
        return;
    }

    // check if script is executable
    if !path.metadata().unwrap().permissions().mode() & 0o111 != 0 {
        eprintln!("Script is not executable: {}", path.display());
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
