//! Finalize mock expectations without replacing a test body's returned error.

use std::future::Future;
use wiremock::MockServer;

/// Keep owned servers outside the body so `?` reaches finalization before `Drop`.
pub async fn run<T, E>(
    servers: &[&MockServer],
    body: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let result = body.await;
    for server in servers {
        if result.is_ok() {
            server.verify().await;
        } else {
            match server.received_requests().await {
                Some(requests) => {
                    eprintln!(
                        "Mock server {} recorded {} requests before the test body failed (first 16 paths):",
                        server.uri(),
                        requests.len()
                    );
                    // Exclude headers, query strings and bodies: diagnostics must not
                    // expose credentials or flood output while preserving the primary error.
                    for (index, request) in requests.iter().take(16).enumerate() {
                        eprintln!(
                            "Request #{}: {} {:.256}",
                            index + 1,
                            request.method,
                            request.url.path()
                        );
                    }
                }
                None => eprintln!(
                    "Mock server {}: request recording is disabled",
                    server.uri()
                ),
            }
            // A returned Err is not panic unwinding: Wiremock's Drop would otherwise
            // replace it with an unmet-expectation panic. Record requests before reset.
            server.reset().await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::responses::mount_sse_sequence;
    use crate::responses::start_mock_server;
    use futures::FutureExt;
    use std::future::ready;
    use std::io::Error;
    use std::panic::AssertUnwindSafe;
    use std::ptr;

    #[tokio::test]
    async fn finalization_preserves_errors_without_weakening_counts() {
        let server = start_mock_server().await;
        mount_sse_sequence(&server, vec![String::new()]).await;
        let error = Box::new(Error::other("injected body failure"));
        let identity = ptr::from_ref(error.as_ref());

        // Setup can fail after an earlier server is armed but before another
        // server exists. Every owned server must finalize before its Drop.
        let Err(returned) = run(&[&server], async {
            ready(Err::<(), _>(error)).await?;
            let _later_server = start_mock_server().await;
            Ok::<(), Box<Error>>(())
        })
        .await
        else {
            panic!("the setup error was lost");
        };
        assert_eq!(ptr::from_ref(returned.as_ref()), identity);
        server.verify().await;

        mount_sse_sequence(&server, vec![String::new()]).await;
        let Err(returned) = run(&[&server], async {
            let second = start_mock_server().await;
            mount_sse_sequence(&second, vec![String::new()]).await;
            let third = start_mock_server().await;
            mount_sse_sequence(&third, vec![String::new()]).await;
            let result = run(&[&second, &third], async {
                Err::<(), _>(returned)?;
                Ok::<(), Box<Error>>(())
            })
            .await;
            second.verify().await;
            third.verify().await;
            result
        })
        .await
        else {
            panic!("the body error was lost");
        };
        assert_eq!(ptr::from_ref(returned.as_ref()), identity);
        server.verify().await;

        let unsatisfied = start_mock_server().await;
        mount_sse_sequence(&unsatisfied, vec![String::new()]).await;
        let verification =
            AssertUnwindSafe(run(&[&server, &unsatisfied], ready(Ok::<_, Error>(()))))
                .catch_unwind()
                .await;
        // Disarm before inspecting the caught panic: Drop is no longer unwinding.
        unsatisfied.reset().await;
        let Err(panic) = verification else {
            panic!("a successful body bypassed its exact-count expectation");
        };
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        assert!(message.contains("Expected range of matching incoming requests: == 1"));
        assert!(message.contains("Number of matched incoming requests: 0"));
    }
}
