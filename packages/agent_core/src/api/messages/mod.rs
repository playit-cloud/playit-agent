pub use account::*;
pub use agent::*;
pub use login::*;

mod agent;
mod account;
mod login;

pub trait ApiRequest {
    type Response;

    fn endpoint() -> &'static str;
}