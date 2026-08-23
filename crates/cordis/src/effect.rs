pub trait Disposable: Send + 'static {
    fn dispose(self: Box<Self>);
}

impl<F> Disposable for F
where
    F: FnOnce() + Send + 'static,
{
    fn dispose(self: Box<Self>) {
        (*self)()
    }
}
