# Mini Tokio

Based on the [Mini Tokio](https://tokio.rs/tokio/tutorial/async#mini-tokio) described in the
[Tokio Tutorial](https://tokio.rs/tokio/tutorial), this is a simple implementation of an `async`
executor in Rust.

The original [mini-tokio](https://github.com/tokio-rs/website/blob/master/tutorial-code/mini-tokio/src/main.rs) is
extended to support multiple threads and a [`JoinHandle`].

## Running

```
cargo run --example main
```
