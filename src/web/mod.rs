// Mechanical split of the former `web.rs`.
// Fragments are included in original order to preserve behavior and private helper visibility.
include!("server.rs");
include!("http.rs");
include!("routes.rs");
include!("conduct.rs");
include!("artifacts.rs");
include!("assets.rs");
include!("tests.rs");
