pub mod envoy;
pub mod hyper;
pub mod pbjson;

#[macro_export]
macro_rules! get_or_insert_default {
    ($expr:expr, $path:path) => {
        if let Some($path(value)) = $expr {
            value
        } else {
            *$expr = Some($path(::std::default::Default::default()));
            let Some($path(value)) = $expr else {
                unreachable!()
            };
            value
        };
    };
}
