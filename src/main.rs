use mini_tokio::{time::Delay, MiniTokio};
use std::time::{Duration, Instant};

fn main() {
    let mini_tokio = MiniTokio::new();
    let mini_tokio_inner = MiniTokio::new();

    mini_tokio.spawn(async move {
        // let i = 1;
        for i in 0..50 {
            let jh = mini_tokio_inner.spawn(async move {
                println!("delay {} started!", i);
                let when = Instant::now() + Duration::from_millis(100 * i);
                let future = Delay::new(when);

                let out = future.await;
                assert_eq!(out, "done");
                println!("delay {} completed; out = {:?}", i, out);
                out
            });
            let out = jh.await.unwrap();
            println!("out after JoinHandle = {:?}", out);
        }
        mini_tokio_inner.run(num_cpus::get());
    });

    println!("Running mini_tokio...");
    mini_tokio.run(num_cpus::get());
    println!("mini_tokio finished!");
}
