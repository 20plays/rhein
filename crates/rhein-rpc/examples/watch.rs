//! Prints what Rhein would share, for trying the sources by hand:
//!
//!   cargo run -p rhein-rpc --example watch
//!
//! Start a game, play a song or connect a Discord RPC app and watch the activities change.

#[tokio::main]
async fn main() {
    env_logger::init();
    let cache = std::env::temp_dir().join("rhein-rpc-watch");
    let handle = rhein_rpc::start(rhein_rpc::Config::new(cache));
    let (mut activities, mut status) = (handle.activities.clone(), handle.status.clone());
    loop {
        tokio::select! {
            Ok(()) = activities.changed() => {
                let now = activities.borrow_and_update().clone();
                if now.is_empty() {
                    println!("activities: none");
                }
                for a in now {
                    println!("activity: {} — {}", a.summary(), serde_json::to_string(&a).unwrap_or_default());
                }
            }
            Ok(()) = status.changed() => println!("status: {:?}", status.borrow_and_update()),
        }
    }
}
