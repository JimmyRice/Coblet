use std::future::Future;
use std::pin::Pin;

/// Boxed, sendable future returned by methods of object-safe extension traits.
///
/// Extension points are stored as `Arc<dyn Trait>`, and a trait with `async fn`
/// methods is not object safe. Those methods return this alias instead.
///
/// # Examples
///
/// ```
/// use coblet_runtime::BoxFuture;
///
/// trait Greeter: Send + Sync {
///     fn greet(&self) -> BoxFuture<'_, String>;
/// }
///
/// struct English;
///
/// impl Greeter for English {
///     fn greet(&self) -> BoxFuture<'_, String> {
///         Box::pin(async { "hello".to_owned() })
///     }
/// }
///
/// let greeters: Vec<Box<dyn Greeter>> = vec![Box::new(English)];
/// # drop(greeters);
/// ```
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
