use prost::Message;

#[derive(Message, PartialEq)]
pub struct HelloWorld {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(string, repeated, tag = "2")]
    pub messages: Vec<String>,
}
