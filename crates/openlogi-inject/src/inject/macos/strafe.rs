//! Delegation of adjacent-Space switches to the Strafe menu-bar app.
//!
//! When Strafe is running it owns `/tmp/strafe.sock` and switches the Space of
//! the display under the pointer with its own transition speed. Only an
//! unavailable endpoint permits OpenLogi's native switch. Once connected,
//! Strafe owns the request: a rejection can be its per-display boundary guard,
//! and a lost acknowledgement can follow a successful post. Retrying either
//! natively can switch a different display or repeat a delivered switch.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::super::space_switch::Direction;

const STRAFE_CONTROL_SOCKET: &str = "/tmp/strafe.sock";
/// Long enough for Strafe to post the switch and acknowledge it locally.
const STRAFE_CONTROL_TIMEOUT: Duration = Duration::from_millis(100);

/// Hand `direction` to Strafe. Returns `true` when Strafe accepted the
/// connection, in which case the caller must not switch natively.
pub(super) fn delegate(direction: Direction) -> bool {
    delegate_over(direction, UnixStream::connect(STRAFE_CONTROL_SOCKET))
}

fn delegate_over(direction: Direction, connection: std::io::Result<UnixStream>) -> bool {
    let Ok(mut stream) = connection else {
        return false;
    };

    match send_request(&mut stream, direction) {
        Ok(()) => tracing::debug!(?direction, "Strafe acknowledged Space switch"),
        Err(error) => tracing::warn!(
            ?direction,
            %error,
            "Strafe did not acknowledge Space switch; suppressing native retry"
        ),
    }
    true
}

fn send_request(stream: &mut UnixStream, direction: Direction) -> std::io::Result<()> {
    stream.set_read_timeout(Some(STRAFE_CONTROL_TIMEOUT))?;
    stream.set_write_timeout(Some(STRAFE_CONTROL_TIMEOUT))?;
    stream.write_all(command(direction))?;
    let mut response = [0_u8; 3];
    stream.read_exact(&mut response)?;
    if response != *b"ok\n" {
        return Err(std::io::Error::other("Strafe rejected the switch request"));
    }
    Ok(())
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

    #[test]
    fn timeout_allows_local_switch_round_trip() {
        assert!(STRAFE_CONTROL_TIMEOUT >= Duration::from_millis(50));
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
            ));
        }
    }

    #[test]
    fn acknowledgement_and_boundary_rejection_both_suppress_native_retry() {
        for response in [b"ok\n".as_slice(), b"error switch failed\n"] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server.write_all(response).unwrap();
            assert!(delegate_over(Direction::Next, Ok(client)));
            let mut request = String::new();
            server.read_to_string(&mut request).unwrap();
            assert_eq!(request, "switch right\n");
        }
    }

    #[test]
    fn missing_acknowledgement_does_not_repeat_a_delivered_switch() {
        let (client, mut server) = UnixStream::pair().unwrap();
        // Keep the peer open without replying, as when Strafe posts a switch
        // but the acknowledgement misses OpenLogi's deadline.
        assert!(delegate_over(Direction::Previous, Ok(client)));
        let mut request = String::new();
        server.read_to_string(&mut request).unwrap();
        assert_eq!(request, "switch left\n");
    }

    #[test]
    fn truncated_reply_does_not_trigger_native_retry() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"ok").unwrap();
        server.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(delegate_over(Direction::Next, Ok(client)));
    }
}
