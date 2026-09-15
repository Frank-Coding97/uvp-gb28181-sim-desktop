//! Windows transport for FFmpeg encoder timing statistics.
//!
//! FFmpeg's `-stats_enc_post` option can write the encoded frame PTS to a TCP
//! socket on Windows.  The capture thread owns [`StatsReader`]; this module
//! deliberately does not create another worker thread for the socket.

use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const ACCEPT_DEADLINE: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// A loopback reader used by FFmpeg's `-stats_enc_post` output.
///
/// The listener is bound to an ephemeral port and advertised as a loopback
/// `tcp://` URL.  The capture thread owns this reader, so cancellation can be
/// observed without introducing another worker thread just for the socket.
pub(crate) struct StatsReader {
    listener: TcpListener,
    stream: Option<TcpStream>,
    stop: Arc<AtomicBool>,
    url: String,
    accept_deadline: Instant,
}

impl StatsReader {
    /// Bind an ephemeral loopback endpoint for FFmpeg timing statistics.
    pub(crate) fn bind(stop: Arc<AtomicBool>) -> io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("tcp://{}", listener.local_addr()?);
        Ok(Self {
            listener,
            stream: None,
            stop,
            url,
            accept_deadline: Instant::now() + ACCEPT_DEADLINE,
        })
    }

    /// Return the endpoint passed to FFmpeg's `-stats_enc_post` option.
    pub(crate) fn url(&self) -> String {
        self.url.clone()
    }
}

impl Read for StatsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.stop.load(Ordering::Acquire) {
            return Ok(0);
        }

        if self.stream.is_none() {
            loop {
                if self.stop.load(Ordering::Acquire) {
                    return Ok(0);
                }
                match self.listener.accept() {
                    Ok((stream, _peer)) => {
                        // The nonblocking listener makes accept cancellable. Once
                        // connected, use a short blocking read timeout so a quiet
                        // FFmpeg process still gives us regular stop checks.
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(READ_TIMEOUT))?;
                        self.stream = Some(stream);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= self.accept_deadline {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "FFmpeg timing stats connection was not established within 15 seconds",
                            ));
                        }
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(error) => return Err(error),
                }
            }
        }

        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(0);
            }
            let stream = self
                .stream
                .as_mut()
                .expect("timing stats stream is set before reading");
            match stream.read(buf) {
                Ok(0) => return Ok(0),
                Ok(read) => {
                    // Do not publish a final chunk after cancellation. The
                    // caller treats EOF as the signal to release its capture
                    // resources and stop consuming timing lines.
                    return if self.stop.load(Ordering::Acquire) {
                        Ok(0)
                    } else {
                        Ok(read)
                    };
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.stop.load(Ordering::Acquire) {
                        return Ok(0);
                    }
                    // A timeout is expected while FFmpeg is between encoded
                    // frames. Retry only these two transient socket errors;
                    // every other error is returned to the timing thread.
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    fn endpoint(reader: &StatsReader) -> std::net::SocketAddr {
        reader
            .url()
            .strip_prefix("tcp://")
            .expect("timing URL must use tcp://")
            .parse()
            .expect("timing URL must contain a socket address")
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn stats_reader_is_send_and_binds_loopback_ephemeral_endpoint() {
        assert_send::<StatsReader>();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = StatsReader::bind(stop).expect("loopback bind");
        let endpoint = endpoint(&reader);
        assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert_ne!(endpoint.port(), 0);
        assert_eq!(reader.url(), format!("tcp://{endpoint}"));
    }

    #[test]
    fn loopback_reads_multiple_stats_lines() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = StatsReader::bind(stop).expect("loopback bind");
        let mut client = TcpStream::connect(endpoint(&reader)).expect("connect stats socket");
        client
            .write_all(b"0 1/10\n1 1/10\n2 1/10\n")
            .expect("write stats lines");
        drop(client);

        let mut received = Vec::new();
        reader.read_to_end(&mut received).expect("read stats lines");
        assert_eq!(received, b"0 1/10\n1 1/10\n2 1/10\n");
    }

    #[test]
    fn stop_cancels_read_before_a_stats_connection_arrives() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = StatsReader::bind(Arc::clone(&stop)).expect("loopback bind");
        let barrier = Arc::new(Barrier::new(2));
        let (done_tx, done_rx) = mpsc::channel();
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            let mut buf = [0_u8; 64];
            done_tx
                .send(reader.read(&mut buf))
                .expect("report read result");
        });

        barrier.wait();
        thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Release);
        let result = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop must wake an unconnected read");
        worker.join().expect("reader thread must exit");
        assert_eq!(result.expect("stop is an EOF"), 0);
    }

    #[test]
    fn stop_cancels_read_after_connection_when_peer_is_quiet() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = StatsReader::bind(Arc::clone(&stop)).expect("loopback bind");
        let client = TcpStream::connect(endpoint(&reader)).expect("connect stats socket");
        let barrier = Arc::new(Barrier::new(2));
        let (done_tx, done_rx) = mpsc::channel();
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            let mut buf = [0_u8; 64];
            done_tx
                .send(reader.read(&mut buf))
                .expect("report read result");
        });

        barrier.wait();
        thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Release);
        let result = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop must wake a quiet connected read");
        worker.join().expect("reader thread must exit");
        drop(client);
        assert_eq!(result.expect("stop is an EOF"), 0);
    }

    #[test]
    fn peer_disconnect_is_returned_as_real_eof() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = StatsReader::bind(stop).expect("loopback bind");
        let client = TcpStream::connect(endpoint(&reader)).expect("connect stats socket");
        drop(client);

        let started = Instant::now();
        let mut buf = [0_u8; 64];
        let read = reader.read(&mut buf).expect("peer EOF");
        assert_eq!(read, 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
