pub const ORIGIN: &str = "https://hello.iitk.ac.in";

#[derive(Debug)]
pub struct AccessDenied;

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session expired or access denied. Sign in again to continue"
        )
    }
}

impl std::error::Error for AccessDenied {}
