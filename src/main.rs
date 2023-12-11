use mini_tokio::{Delay, MiniTokio};
use std::time::{Duration, Instant};

fn main() {
    let mini_tokio = MiniTokio::new();

    mini_tokio.spawn(async {
        let when = Instant::now() + Duration::from_millis(10);
        let future = Delay::new(when);

        let out = future.await;
        assert_eq!(out, "done");
        println!("delay completed; out = {:?}", out);
    });

    println!("Running mini_tokio...");
    mini_tokio.run();
}
