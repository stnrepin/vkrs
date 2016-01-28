use std::fmt;
use std::ops::Deref;
use std::marker::PhantomData;
use std::error::Error as StdError;
use std::result::Result as StdResult;
use serde::de;
use serde_json::{self, Error as JsonError};
use hyper::client::Client as HttpClient;
use hyper::Error as HttpError;
use url::{self, ParseError as UrlError, Url};
use oauth2::token::Token;

use super::auth::{AccessToken, OAuth};

pub const VK_DOMAIN: &'static str = "api.vk.com";
pub const VK_PATH: &'static str = "method";

#[cfg(feature = "nightly")]
include!("api.rs.in");

#[cfg(not(feature = "nightly"))]
include!(concat!(env!("OUT_DIR"), "/api.rs"));

pub struct Client<'a> {
    client: HttpClient,
    token: Option<&'a AccessToken>,
}

#[derive(Debug)]
pub enum Error {
    Api(VkError),
    Http(HttpError),
    Json(JsonError),
}

impl ::std::fmt::Display for Error {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        match *self {
            Error::Api(ref err) => err.fmt(f),
            Error::Http(ref err) => err.fmt(f),
            Error::Json(ref err) => err.fmt(f),
        }
    }
}

impl From<VkError> for Error {
    fn from(err: VkError) -> Error {
        Error::Api(err)
    }
}

impl From<HttpError> for Error {
    fn from(err: HttpError) -> Error {
        Error::Http(err)
    }
}

impl From<JsonError> for Error {
    fn from(err: JsonError) -> Error {
        Error::Json(err)
    }
}

impl From<UrlError> for Error {
    fn from(err: UrlError) -> Error {
        Error::Http(HttpError::Uri(err))
    }
}

pub type Result<T> = StdResult<T, Error>;

impl<'a> Client<'a> {
    pub fn auth<K, S>(key: K, secret: S) -> OAuth where K: Into<String>, S: Into<String> {
        OAuth::new(
            Default::default(),
            key.into(),
            secret.into(),
            Some(String::from(::auth::OAUTH_DEFAULT_REDIRECT_URI)))
    }

    pub fn new() -> Client<'a> {
        Client {
            client: HttpClient::new(),
            token: None,
        }
    }

    pub fn token(&mut self, token: &'a AccessToken) -> &mut Self {
        self.token = Some(token);
        self
    }

    pub fn get<T: Request>(&mut self, req: &T) -> Result<T::Response> {
        let mut url = req.to_url();
        if let Some(ref token) = self.token {
            if let Some(ref mut query) = url.query {
                query.push_str("&access_token=");
                query.push_str(token.access_token());
            } else {
                url.query = Some(String::from("access_token=")
                                 + token.access_token());
            }
        }

        self.client
            .get(url)
            .send()
            .map_err(Error::Http)
            .and_then(|resp| {
                serde_json::from_reader::<_, VkResult<T::Response>>(resp)
                    .map_err(Error::Json)
            })
            .and_then(|vkres| vkres.0.map_err(Error::Api))
    }
}

/// Trait for things that can be posted to VK API directly
pub trait Request {
    type Response: de::Deserialize;
    fn method_name() -> &'static str;
    fn to_query_string(&self) -> String;

    fn to_url(&self) -> Url {
        Url {
            scheme: String::from("https"),
            scheme_data: url::SchemeData::Relative(url::RelativeSchemeData {
                username: String::new(),
                password: None,
                host: url::Host::Domain(String::from(VK_DOMAIN)),
                port: None,
                default_port: Some(443),
                path: vec![String::from(VK_PATH), String::from(Self::method_name())]
                }),
            query: Some(self.to_query_string()),
            fragment: None,
        }
    }

}

#[derive(Debug)]
pub struct VkResult<T>(pub StdResult<T, VkError>);

impl<T> Deref for VkResult<T> {
    type Target = StdResult<T, VkError>;
    fn deref(&self) -> &StdResult<T, VkError> {
        &self.0
    }
}

enum VkResultField {
    Response,
    Error
}

impl de::Deserialize for VkResultField {
    fn deserialize<D: de::Deserializer>(d: &mut D) -> StdResult<VkResultField, D::Error> {
        struct VkResultFieldVisitor;

        impl de::Visitor for VkResultFieldVisitor {
            type Value = VkResultField;
            fn visit_str<E: de::Error>(&mut self, value: &str) -> StdResult<VkResultField, E> {
                match value {
                    "response" => Ok(VkResultField::Response),
                    "error" => Ok(VkResultField::Error),
                    _ => Err(de::Error::syntax("expected response or error"))
                }
            }
        }

        d.visit(VkResultFieldVisitor)
    }
}

impl<T: de::Deserialize> de::Deserialize for VkResult<T> {
    fn deserialize<D: de::Deserializer>(d: &mut D) -> StdResult<VkResult<T>, D::Error> {
        struct VkResultVisitor<T: de::Deserialize>(PhantomData<T>);

        impl<T: de::Deserialize> de::Visitor for VkResultVisitor<T> {
            type Value = VkResult<T>;
            fn visit_map<V: de::MapVisitor>(&mut self, mut v: V) -> StdResult<VkResult<T>, V::Error> {
                v.visit_key()
                 .and_then(|k| k.map(|k| match k {
                    VkResultField::Response => v.visit_value::<T>().map(Ok),
                    VkResultField::Error => v.visit_value::<VkError>().map(Err),
                 }).unwrap_or_else(|| v.missing_field("response or error")))
                 .and_then(|res| v.end().map(|_| res))
                 .map(VkResult)
            }
        }

        d.visit_map(VkResultVisitor(PhantomData::<T>))
    }
}

impl Into<(String, String)> for KeyVal {
    fn into(self) -> (String, String) {
        (self.key, self.value)
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum VkErrorCode {
    General, // 1
    Database, // 2
    Unauthorized, // 5
    Signature, // 10
    Request, // 11
    Blocked, // 19
    GoodsNotFound, // 20
    GoodsUnvailable, // 21
    UserNotFound, // 22
    AccessDenied, // 204
    App(u32), // 100-999
    Unknown(u32), // other
}

impl From<u32> for VkErrorCode {
    fn from(value: u32) -> VkErrorCode {
        use self::VkErrorCode::*;
        match value {
            1 => General,
            2 => Database,
            5 => Unauthorized,
            10 => Signature,
            11 => Request,
            19 => Blocked,
            20 => GoodsNotFound,
            21 => GoodsUnvailable,
            22 => UserNotFound,
            204 => AccessDenied,
            v @ 100...999 => App(v),
            v @ _ => Unknown(v)
        }
    }
}
impl Into<u32> for VkErrorCode {
    fn into(self) -> u32 {
        use self::VkErrorCode::*;
        match self {
            General => 1,
            Database => 2,
            Unauthorized => 5,
            Signature => 10,
            Request => 11,
            Blocked => 19,
            GoodsNotFound => 20,
            GoodsUnvailable => 21,
            UserNotFound => 22,
            AccessDenied => 204,
            App(v) => v,
            Unknown(v) => v,
        }
    }
}

impl fmt::Display for VkErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::VkErrorCode::*;
        match *self {
            General => f.write_str("general error"),
            Database => f.write_str("database error"),
            Unauthorized => f.write_str("unauthorized"),
            Signature => f.write_str("invalid signature"),
            Request => f.write_str("invalid request"),
            Blocked => f.write_str("content blocked"),
            GoodsNotFound => f.write_str("goods not found"),
            GoodsUnvailable => f.write_str("goods unavailable"),
            UserNotFound => f.write_str("user not found"),
            AccessDenied => f.write_str("access denied"),
            App(v) => write!(f, "application error #{}", v),
            Unknown(v) => write!(f, "unknown error #{}", v),
        }
    }
}

impl de::Deserialize for VkErrorCode {
    fn deserialize<D: de::Deserializer>(d: &mut D) -> StdResult<VkErrorCode, D::Error> {
        u32::deserialize(d).map(From::from)
    }
}

impl StdError for VkError {
    fn description(&self) -> &str {
        &*self.error_msg
    }
}

impl fmt::Display for VkError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.error_code, self.error_msg)
    }
}

pub enum Privacy {
    All,
    Friends,
    FriendsOfFriends,
    FriendsOfFriendsOnly,
    Nobody,
    OnlyMe,
    List(u64),
    OnlyList(u64),
    User(u64),
    OnlyUser(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Sort {
    DateAdded = 0,
    Length = 1,
    Popularity = 2,
}

impl AsRef<str> for Sort {
    fn as_ref(&self) -> &str {
        use self::Sort::*;
        match *self {
            DateAdded => "0",
            Length => "1",
            Popularity => "2",
        }
    }
}

