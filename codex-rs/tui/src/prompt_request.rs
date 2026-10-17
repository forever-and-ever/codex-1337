// Other clients serialize context before this delimiter; replay still displays only the request.
pub(crate) fn extract_prompt_request_with_offset(message: &str) -> (&str, usize) {
    const REQUEST_BEGIN: &str = "## My request for Codex:";
    let Some((before_request, request)) = message.rsplit_once(REQUEST_BEGIN) else {
        return (message, 0);
    };
    let request_start = before_request.len() + REQUEST_BEGIN.len();
    let leading_trimmed_len = request.len() - request.trim_start().len();
    (request.trim(), request_start + leading_trimmed_len)
}
