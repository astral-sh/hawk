use external_trait::ExternalDispatch;

pub fn call_external<T: ExternalDispatch>(value: &T) {
    value.run();
}
