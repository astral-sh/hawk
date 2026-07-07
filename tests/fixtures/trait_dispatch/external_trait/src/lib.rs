pub trait ExternalDispatch {
    fn run(&self);
}

pub fn call_external<T: ExternalDispatch>(value: &T) {
    value.run();
}
