//! Delegation of adjacent-Space switches to the Strafe menu-bar app.
//!
//! When Strafe is running it owns `/tmp/strafe.sock` and switches the Space of
//! the display under the pointer with its own transition speed. Only an
//! unavailable endpoint permits OpenLogi's native switch. Once connected,
//! Strafe owns the request: a rejection can be its per-display boundary guard,
//! and a late reply can follow a delivered switch. Retrying either natively
//! can switch a different display or repeat a delivered switch.
//!
//! Strafe replies once the Dock has carried the switch out: `ok` when it
//! landed, `error <reason>` otherwise. Its gate posts one switch at a time, so
//! the reply to a request queued behind others can take well over 100 ms. The
//! request is written on the caller's thread, which keeps rapid requests in
//! order; the reply is awaited on a thread of its own, so a pending switch
//! never holds up the action that follows it.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::super::space_switch::Direction;

const STRAFE_CONTROL_SOCKET: &str = "/tmp/strafe.sock";
/// Writing one short request to a local socket.
const STRAFE_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
/// Strafe waits up to 1 s for the Dock to land a switch and drops a request
/// that queued for over 1 s, so a healthy reply comes within 2 s.
const STRAFE_REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest reply Strafe sends (`error post failed\n` is 18 bytes).
const STRAFE_REPLY_LIMIT: usize = 64;

/// Hand `direction` to Strafe. Returns `true` when Strafe accepted the
/// connection, in which case the caller must not switch natively.
pub(super) fn delegate(direction: Direction) -> bool {
    delegate_over(
        direction,
        UnixStream::connect(STRAFE_CONTROL_SOCKET),
        STRAFE_REPLY_TIMEOUT,
    )
}

fn delegate_over(
    direction: Direction,
    connection: std::io::Result<UnixStream>,
    reply_timeout: Duration,
) -> bool {
    let Ok(mut stream) = connection else {
        return false;
    };

    if let Err(error) = send_request(&mut stream, direction) {
        tracing::warn!(
            ?direction,
            %error,
            "could not send Space switch to Strafe; suppressing native retry"
        );
        return true;
    }
    let waiter = std::thread::Builder::new()
        .name("strafe-reply".into())
        .spawn(move || report(direction, read_reply(&mut stream, reply_timeout)));
    if let Err(error) = waiter {
        tracing::warn!(?direction, %error, "could not wait for Strafe's reply");
    }
    true
}

fn send_request(stream: &mut UnixStream, direction: Direction) -> std::io::Result<()> {
    stream.set_write_timeout(Some(STRAFE_WRITE_TIMEOUT))?;
    stream.write_all(command(direction))
}

/// One reply line without its newline. A reply cut off before the newline
/// is an error: Strafe always ends its replies with one.
fn read_reply(stream: &mut UnixStream, timeout: Duration) -> std::io::Result<String> {
    stream.set_read_timeout(Some(timeout))?;
    let mut reply = Vec::new();
    let mut buffer = [0_u8; STRAFE_REPLY_LIMIT];
    while reply.len() < STRAFE_REPLY_LIMIT {
        let count = stream.read(&mut buffer[..STRAFE_REPLY_LIMIT - reply.len()])?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        reply.extend_from_slice(&buffer[..count]);
        if let Some(end) = reply.iter().position(|&byte| byte == b'\n') {
            reply.truncate(end);
            return Ok(String::from_utf8_lossy(&reply).into_owned());
        }
    }
    Err(std::io::Error::other("Strafe's reply is too long"))
}

fn report(direction: Direction, reply: std::io::Result<String>) {
    match reply {
        Ok(reply) if reply == "ok" => tracing::debug!(?direction, "Strafe landed Space switch"),
        // The boundary guard and a lost switch are Strafe's answers, not faults.
        Ok(reply) => tracing::debug!(?direction, %reply, "Strafe did not land Space switch"),
        Err(error) => tracing::warn!(
            ?direction,
            %error,
            "Strafe did not answer Space switch; suppressing native retry"
        ),
    }
}

fn command(direction: Direction) -> &'static [u8] {
    match direction {
        Direction::Previous => b"switch left\n",
        Direction::Next => b"switch right\n",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_space_directions() {
        assert_eq!(command(Direction::Previous), b"switch left\n");
        assert_eq!(command(Direction::Next), b"switch right\n");
    }

    const SHORT: Duration = Duration::from_millis(50);

    #[test]
    fn reply_timeout_covers_strafes_gate() {
        // 1 s landing timeout plus 1 s maximum queueing in Strafe.
        assert!(STRAFE_REPLY_TIMEOUT >= Duration::from_secs(2));
    }

    #[test]
    fn delegation_does_not_wait_for_the_reply() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        assert!(delegate_over(
            Direction::Next,
            Ok(client),
            Duration::from_secs(2)
        ));
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "the caller must not block on a reply that comes after landing"
        );
        let mut request = [0_u8; 13];
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"switch right\n");
    }

    #[test]
    fn replies_are_read_to_their_newline() {
        for (sent, read) in [
            (b"ok\n".as_slice(), "ok"),
            (b"error at edge\n", "error at edge"),
            (b"error not landed\n", "error not landed"),
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            server.write_all(sent).unwrap();
            assert_eq!(read_reply(&mut client, SHORT).unwrap(), read);
        }
    }

    #[test]
    fn a_reply_that_never_comes_times_out() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        read_reply(&mut client, SHORT).unwrap_err();
    }

    #[test]
    fn unavailable_endpoint_allows_native_switch() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::ConnectionRefused,
        ] {
            assert!(!delegate_over(
                Direction::Next,
                Err(std::io::Error::from(kind)),
                SHORT,
            ));
        }
    }

    #[test]
    fn acknowledgement_and_boundary_rejection_both_suppress_native_retry() {
        for response in [b"ok\n".as_slice(), b"error at edge\n"] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server.write_all(response).unwrap();
            assert!(delegate_over(Direction::Next, Ok(client), SHORT));
            let mut request = String::new();
            server.read_to_string(&mut request).unwrap();
            assert_eq!(request, "switch right\n");
        }
    }

    #[test]
    fn missing_acknowledgement_does_not_repeat_a_delivered_switch() {
        let (client, mut server) = UnixStream::pair().unwrap();
        // Keep the peer open without replying, as when Strafe posts a switch
        // but its reply misses OpenLogi's deadline.
        assert!(delegate_over(Direction::Previous, Ok(client), SHORT));
        let mut request = String::new();
        server.read_to_string(&mut request).unwrap();
        assert_eq!(request, "switch left\n");
    }

    #[test]
    fn truncated_reply_does_not_trigger_native_retry() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"ok").unwrap();
        server.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(delegate_over(Direction::Next, Ok(client), SHORT));
    }
}
