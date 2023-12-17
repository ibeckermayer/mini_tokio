use mini_tokio::{time::Delay, MiniTokio};
use std::time::{Duration, Instant};

fn main() {
    let mini_tokio = MiniTokio::new();

    let i = 0;
    // for i in 0..50 {
    mini_tokio.spawn(async move {
        println!("delay {} started!", i);
        let when = Instant::now() + Duration::from_millis(10 * i);
        let future = Delay::new(when);

        let out = future.await;
        assert_eq!(out, "done");
        println!("delay {} completed; out = {:?}", i, out);
    });
    // }

    println!("Running mini_tokio...");
    mini_tokio.run();
}
