use crate::client;

#[derive(Debug)]
pub struct Endpoint {
    pub id: uuid::Uuid,
    pub client: client::Client,
    pub providers: Vec<schemas::Provider>,
}
