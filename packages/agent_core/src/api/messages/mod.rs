pub use account::*;
pub use agent::*;
pub use login::*;

mod agent;
mod account;
mod login;

pub trait ApiRequest {
    type RequestJson;

    type ResponseJson;
    type Response;

    fn to_req(self) -> Self::RequestJson;

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response>;

    fn endpoint() -> &'static str;
}

pub trait SimpleApiRequest {
    type Response;

    fn endpoint() -> &'static str;
}

impl<T: SimpleApiRequest> ApiRequest for T {
    type RequestJson = T;

    type ResponseJson = T::Response;
    type Response = T::Response;

    fn to_req(self) -> Self::RequestJson {
        self
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        Some(parsed)
    }

    fn endpoint() -> &'static str {
        T::endpoint()
    }
}