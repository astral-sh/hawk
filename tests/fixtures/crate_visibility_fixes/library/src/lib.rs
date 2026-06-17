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
        let _ = crate::trait_impl::Runtime;
    }
}

mod trait_api {
    pub(crate) trait Approvable {
        type ApprovalKey;
    }
}

mod trait_impl {
    pub(crate) struct ApprovalKey;
    pub(crate) struct Runtime;

    impl crate::trait_api::Approvable for Runtime {
        type ApprovalKey = ApprovalKey;
    }
}
