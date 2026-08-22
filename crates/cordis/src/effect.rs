use crate::context::Context;

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

pub trait Effect: Send + Sync + 'static {
    fn apply(&self, ctx: &Context) -> Box<dyn Disposable>;
}

// EffectGuard reverses on Drop (LIFO)
pub struct EffectGuard {
    acc: Vec<Box<dyn FnOnce() + Send>>,
}

impl EffectGuard {
    pub fn new() -> Self {
        Self { acc: Vec::new() }
    }
    pub fn push(&mut self, undo: Box<dyn FnOnce() + Send>) {
        self.acc.push(undo);
    }
}

impl Default for EffectGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EffectGuard {
    fn drop(&mut self) {
        while let Some(undo) = self.acc.pop() {
            undo();
        }
    }
}
