# ntfy-rs: a client for ntfy

This project was created to minimize the concurent connections to the ntfy server. 
During an unrelated issue I discovered that the ntfy desktop client was creating a new connection for every subscription.

This project uses the "Subscribe to multiple topics" feature of the ntfy server to minimize the number of connections.
This is done by using comma separated values in the topic field of the subscription, and parsing the incoming topic for command execution instead of giving each topic a thread.

Currently the client only supports the existing ntfy configuration file, and does not support any command line arguments.
The goal is to support the same configuration file as the ntfy client, and to support the same command line arguments. For now the focus is on the configuration file.

## Usage

```bash
git clone https://github.com/winneratwin/ntfy-rust-client.git
cargo run --release
```
