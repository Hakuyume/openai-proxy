pub fn from_msg<M>(msg: &M) -> Result<pbjson_types::Any, prost::EncodeError>
where
    M: prost::Name,
{
    prost_types::Any::from_msg(msg).map(|prost_types::Any { type_url, value }| pbjson_types::Any {
        type_url,
        value: value.into(),
    })
}
