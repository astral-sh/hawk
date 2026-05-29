pub fn entry() {
    scoped::run();
    sibling::call();
}

mod scoped {
    pub(crate) fn run() {
        private_helper();
        private_parent_visible_helper();
        private_formatted_helper();
        sibling::call_parent_helper();
    }

    pub(crate) fn private_helper() {}

    pub(super) fn private_parent_visible_helper() {}

    pub /* preserve parsing */ (super) fn private_formatted_helper() {}

    pub(crate) fn parent_helper() {}

    mod sibling {
        pub(crate) fn call_parent_helper() {
            super::parent_helper();
        }
    }
}

mod target {
    pub(crate) fn f() {}
}

mod wrapper {
    pub(crate) mod api {
        pub(crate) use crate::target::f;
    }
}

mod sibling {
    pub(crate) fn call() {
        crate::wrapper::api::f();
    }
}
