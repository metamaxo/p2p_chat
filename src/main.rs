use rand::random_range;
use std::{
    collections::hash_map::DefaultHasher,
    error::Error,
    hash::{Hash, Hasher},
    time::Duration,
};

use futures::stream::StreamExt;
use libp2p::gossipsub::TopicHash;
use libp2p::{
    gossipsub, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use tokio::{io, io::AsyncBufReadExt, select};
use tracing_subscriber::EnvFilter;

pub struct User {
    username: String,
    topic_list: Vec<TopicHash>,
}

impl User {
    fn new() -> Self {
        User {
            username: String::new(),
            topic_list: Vec::new(),
        }
    }
    fn add_username(&mut self, username: String) {
        self.username = username
    }
    fn add_topic(&mut self, topic: TopicHash) {
        self.topic_list.push(topic);
    }
    fn remove_topic(&mut self, topic: &String) {
        let t = TopicHash::from_raw(topic);
        if let Some(index) = self.topic_list.iter().position(|x| *x == t) {
            self.topic_list.remove(index);
        }
    }
}

// We create a custom network behaviour that combines Gossipsub and Mdns.
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}
fn get_username() -> String {
    use std::io::{Write, stdin, stdout};
    let mut username = String::new();
    print!("Please enter username: ");
    let _ = stdout().flush();
    stdin()
        .read_line(&mut username)
        .expect("Did not enter a correct string");
    if let Some('\n') = username.chars().next_back() {
        username.pop();
    }
    if let Some('\r') = username.chars().next_back() {
        username.pop();
    }
    println!("You typed: {}", username);
    username
}

fn get_topic() -> String {
    use std::io::{Write, stdin, stdout};
    let mut topic = String::new();
    print!("Please enter topic: ");
    let _ = stdout().flush();
    stdin()
        .read_line(&mut topic)
        .expect("Did not enter a correct string");
    if let Some('\n') = topic.chars().next_back() {
        topic.pop();
    }
    if let Some('\r') = topic.chars().next_back() {
        topic.pop();
    }
    println!("You typed: {}", topic);
    topic
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut user = User::new();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            // To content-address message, we can take the hash of message and use it as an ID.
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };
            // Set a custom gossipsub configuration
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
                .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message
                // signing)
                .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
                .build()
                .map_err(|msg| io::Error::new(io::ErrorKind::Other, msg))?; // Temporary hack because `build` does not return a proper `std::error::Error`.

            // build a gossipsub network behaviour
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            Ok(MyBehaviour { gossipsub, mdns })
        })?
        .build();

    // Create a Gossipsub topic

    let topic = gossipsub::IdentTopic::new(get_topic());
    // subscribes to our topic
    user.add_topic(topic.hash());
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("Enter messages via STDIN and they will be sent to connected peers using Gossipsub");
    let username = get_username();
    user.add_username(username);
    // Kick it off
    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                if line.starts_with("/unsubscribe") {
                    let t = line.replace("/unsubscribe ", "");
                    user.remove_topic(&t);
                    swarm.behaviour_mut().gossipsub.unsubscribe(&gossipsub::IdentTopic::new(t))?;
                } else if line.starts_with("/subscribe") {
                    let t = line.replace("/subscribe ", "");
                    user.add_topic(TopicHash::from_raw(&t));
                    swarm.behaviour_mut().gossipsub.subscribe(&gossipsub::IdentTopic::new(t))?;
                } else {
                    for topic in &user.topic_list {
                    if let Err(e) = swarm
                    .behaviour_mut().gossipsub
                    .publish(topic.clone(), format!("{}: {}", user.username, line).as_bytes()) {
                    println!("Publish error: {e:?}");
                }}}
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("mDNS discovered a new peer: {peer_id}");
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("mDNS discover peer has expired: {peer_id}");
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    propagation_source: peer_id,
                    message_id: id,
                    message,
                })) => println!("peer_id{}, {}",peer_id, String::from_utf8_lossy(&message.data)),
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Subscribed {
                    peer_id: id,
                    topic: top
                })) => println!("{id} just joined {top}"),
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
}
