pub mod anthropic;
pub mod common;
pub mod fabro_server;
pub mod gemini;
pub mod http_api;
pub mod openai;
pub(crate) mod openai_chat;
pub mod openai_compatible;

pub use anthropic::Adapter as AnthropicAdapter;
pub use fabro_server::Adapter as FabroServerAdapter;
pub use gemini::Adapter as GeminiAdapter;
pub use openai::Adapter as OpenAiAdapter;
pub use openai_compatible::Adapter as OpenAiCompatibleAdapter;
