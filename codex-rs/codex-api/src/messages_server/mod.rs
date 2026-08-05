//! Anthropic `/v1/messages` **server leg**: inbound Anthropic requests routed to
//! downstream `/responses`, with Anthropic-shaped SSE synthesized for the caller.
//!
//! S-ANTHROPIC-SERVER-LEG — inverse of the native outbound Messages client in
//! `endpoint/messages.rs`.

mod inbound;
mod outbound;

pub use inbound::AnthropicInboundRequest;
pub use inbound::system_instructions;
pub use inbound::translate_inbound_to_responses_input;
pub use inbound::translate_inbound_tools;
pub use inbound::translate_tool_choice;
pub use outbound::AnthropicSseEvent;
pub use outbound::synthesize_anthropic_sse_from_response_events;
