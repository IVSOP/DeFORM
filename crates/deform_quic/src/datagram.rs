use std::{
    collections::{BTreeMap, HashMap, hash_map},
    marker::PhantomData,
};

use deform_core::{DeformError, DeformResult};
use quinn::Connection;
use wincode::{SchemaRead, SchemaWrite};

use crate::DeformQuicLogic;

/// One unreliable datagram: a slice of a larger message, plus what the receiver needs
/// to put that message back together.
#[derive(SchemaRead, SchemaWrite, Clone, Debug)]
pub struct DeformDatagram {
    /// Counter owned by whoever sent this datagram.
    ///
    /// One sequence per direction, not per connection: the ids a server sends have
    /// nothing to do with the ids it receives from that same client, and neither side
    /// ever compares one against the other.
    ///
    /// Deliberately not the tick, which repeats while a client is frozen at
    /// `max_ticks_ahead` and would let two different messages share an id.
    message_id: u64,
    fragment_id: u8,
    total_fragments: u8,
    data: Vec<u8>,
}

/// Bytes wincode wraps around `data`, reserved out of every datagram: 8 for
/// `message_id`, 1 each for the two fragment fields, 8 for `data`'s length.
///
/// Constant only because wincode encodes lengths and integers at fixed width.
/// `envelope_overhead_is_covered` measures it for real, so a change to that encoding
/// fails a test instead of silently producing datagrams quinn refuses to send.
const ENVELOPE_OVERHEAD: usize = 18;

/// Sends datagrams over a quinn [`Connection`], automatically fragmenting them as needed.
pub struct DatagramFragmentor<Q: DeformQuicLogic> {
    connection: Connection,
    next_message_id: u64,
    phantom: PhantomData<Q>,
}

impl<Q: DeformQuicLogic> DatagramFragmentor<Q> {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            next_message_id: 0,
            phantom: PhantomData,
        }
    }

    /// Splits `body` and sends every fragment.
    pub fn send(&mut self, body: &[u8]) -> DeformResult<()> {
        // Read the MTU per send: quinn revises its estimate as the path changes, and
        // returns None if the peer turned datagrams off entirely.
        let max_datagram_size = self
            .connection
            .max_datagram_size()
            .ok_or_else(|| DeformError::Connection("peer does not accept datagrams".into()))?;

        let message_id = self.next_message_id;
        self.next_message_id += 1;

        let datagrams = Self::fragment(message_id, body, max_datagram_size)?;
        // Fragmenting is meant to be rare. If this is printing every tick, the state
        // encoding is too big for the path and that is the problem to fix, not this.
        if datagrams.len() > 1 {
            tracing::warn!(
                body_bytes = body.len(),
                fragments = datagrams.len(),
                max_datagram_size,
                "message did not fit one datagram"
            );
        }

        for datagram in datagrams {
            self.connection
                .send_datagram(datagram.into())
                .map_err(|e| DeformError::Connection(e.to_string()))?;
        }

        Ok(())
    }

    /// Splits `body` across as many datagrams as it needs, each a serialized
    /// [`DeformDatagram`].
    ///
    /// `max_datagram_size` is the connection's *current* estimate — quinn revises it as
    /// the path changes, so read it per send rather than caching a constant.
    pub fn fragment(
        message_id: u64,
        body: &[u8],
        max_datagram_size: usize,
    ) -> DeformResult<Vec<Vec<u8>>> {
        split_into_datagrams(message_id, body, max_datagram_size, Q::MAX_FRAGMENTS)
    }
}

/// The body of [`DatagramFragmentor::fragment`], with the one thing it needs from `Q`
/// passed in. Free-standing so it can be exercised without standing up a whole
/// [`DeformQuicLogic`] just to read a constant off it.
fn split_into_datagrams(
    message_id: u64,
    body: &[u8],
    max_datagram_size: usize,
    max_fragments: u8,
) -> DeformResult<Vec<Vec<u8>>> {
    // Must leave at least one byte of body: a zero chunk would divide by zero below,
    // and would not make progress anyway.
    let chunk_size = max_datagram_size.saturating_sub(ENVELOPE_OVERHEAD);
    if chunk_size == 0 {
        return Err(DeformError::Protocol(format!(
            "path MTU allows {max_datagram_size} byte datagrams, leaving no room past the \
            {ENVELOPE_OVERHEAD} byte envelope"
        )));
    }

    // An empty body still owes the peer one datagram, hence the `max(1)`.
    let total_fragments = body.len().div_ceil(chunk_size).max(1);
    if total_fragments > max_fragments as usize {
        return Err(DeformError::Protocol(format!(
            "message of {} bytes needs {total_fragments} fragments, over the {max_fragments} \
            limit; the state encoding is too large for this path",
            body.len()
        )));
    }
    let total_fragments = total_fragments as u8;

    // The metric that says whether this layer is a safety net or the steady state.
    // Anything durably above 1 means the state encoding, not the framing, is the problem.
    #[cfg(feature = "tracy")]
    if let Some(client) = tracy_client::Client::running() {
        client.plot(
            tracy_client::plot_name!("datagram_fragments"),
            total_fragments as f64,
        );
    }

    body.chunks(chunk_size)
        // `chunks` yields nothing for an empty body; this keeps the one-datagram case.
        .chain(body.is_empty().then_some(&body[..0]))
        .enumerate()
        .map(|(fragment_id, chunk)| {
            let datagram = DeformDatagram {
                message_id,
                fragment_id: fragment_id as u8,
                total_fragments,
                data: chunk.to_vec(),
            };
            wincode::serialize(&datagram)
                .map_err(|e| DeformError::Serialize(format!("datagram: {e:?}")).into())
        })
        .collect()
}

/// An incomplete message, possibly containing multiple fragments.
pub struct IncompleteMessage {
    pub message_id: u64,
    pub total_fragments: u8,
    pub fragments: BTreeMap<u8, Vec<u8>>,
}

/// Reads messages from a quinn [`Connection`] and defragments them as needed.
/// You just need to `.await` on [`DatagramDefragmentor::recv`], it will only return
/// messages that have been fully defragmented.
pub struct DatagramDefragmentor<Q: DeformQuicLogic> {
    connection: Connection,
    // key is message_id % 32
    // outer key is message ID, inner key is fragment ID
    // TODO: change to vec to be faster? prob won't matter realistically
    incomplete_messages: HashMap<u8, IncompleteMessage>,
    phantom: PhantomData<Q>,
}

impl<Q: DeformQuicLogic> DatagramDefragmentor<Q> {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            incomplete_messages: HashMap::with_capacity(Q::MAX_MESSAGE_BUFFER as usize),
            phantom: PhantomData,
        }
    }

    /// Waits for a whole message, reading as many datagrams as that takes.
    ///
    /// Cancel-safe: the only await point is quinn's own `read_datagram`, and fragments
    /// already absorbed live in `self`, not in the returned future. Dropping this mid-
    /// flight — which happens on every `select!` where another branch wins — loses
    /// nothing.
    pub async fn recv(&mut self) -> DeformResult<Vec<u8>> {
        loop {
            let bytes = self
                .connection
                .read_datagram()
                .await
                .map_err(|e| DeformError::Connection(e.to_string()))?;

            let datagram: DeformDatagram = wincode::deserialize(&bytes)?;

            // message with no fragments
            if datagram.total_fragments == 0 {
                continue;
            }

            // too many fragments
            if datagram.total_fragments > Q::MAX_FRAGMENTS {
                return Err(DeformError::Protocol(format!(
                    "message claims {} fragments, over the {} limit",
                    datagram.total_fragments,
                    Q::MAX_FRAGMENTS
                )))?;
            }

            // fragment ID larger than specified total fragments
            if datagram.fragment_id >= datagram.total_fragments {
                return Err(DeformError::Protocol(
                    "Message fragment ID is larger than expected".into(),
                ))?;
            }

            if datagram.total_fragments == 1 {
                // no need to even go to the map, just return things instantly
                return Ok(datagram.data);
            } else {
                // I assume the inner conversion is compile time, and that the compiler can optimize the second
                let key = (datagram.message_id % Q::MAX_MESSAGE_BUFFER as u64) as u8;
                match self.incomplete_messages.entry(key) {
                    hash_map::Entry::Vacant(vacant) => {
                        let mut fragments = BTreeMap::new();
                        fragments.insert(datagram.fragment_id, datagram.data);

                        let incomplete_message = IncompleteMessage {
                            message_id: datagram.message_id,
                            total_fragments: datagram.total_fragments,
                            fragments,
                        };

                        vacant.insert(incomplete_message);
                    }
                    hash_map::Entry::Occupied(mut entry) => {
                        let incomplete_message = entry.get_mut();

                        if datagram.message_id != incomplete_message.message_id {
                            if datagram.message_id > incomplete_message.message_id {
                                // this new message is newer. it must entirely replace the entry.
                                incomplete_message.message_id = datagram.message_id;
                                incomplete_message.total_fragments = datagram.total_fragments;
                                incomplete_message.fragments.clear();
                                // insertion still happens bellow, I did not feel like making a fast path here
                            } else {
                                // datagram.message_id < incomplete_message.message_id
                                // this new message is colliding with one that is more recent
                                // the newer message takes priority so the datagram is discarded.
                                continue;
                            }
                        }

                        incomplete_message
                            .fragments
                            .insert(datagram.fragment_id, datagram.data);

                        if incomplete_message.fragments.len() as u8
                            == incomplete_message.total_fragments
                        {
                            let mut bytes = Vec::with_capacity(
                                incomplete_message.fragments.values().map(Vec::len).sum(),
                            );
                            for v in incomplete_message.fragments.values() {
                                bytes.extend_from_slice(v);
                            }

                            entry.remove();
                            return Ok(bytes);
                        }
                    }
                }
            }
        }
    }
}

/// Compresses a serialized body, or hands it back untouched when the game opted out
/// via [`crate::DeformQuicLogic::COMPRESSION`].
pub fn compress(body: Vec<u8>, level: Option<i32>) -> DeformResult<Vec<u8>> {
    let out = match level {
        None => body,
        Some(level) => {
            // Fresh context per call. If this shows up in a profile, the knobs are
            // reusing a `zstd::bulk::Compressor` and training a dictionary on captured
            // snapshots — payloads this small are what dictionaries exist for.
            let compressed = zstd::stream::encode_all(body.as_slice(), level)
                .map_err(|e| DeformError::Serialize(format!("compress datagram: {e}")))?;

            #[cfg(feature = "tracy")]
            if let Some(client) = tracy_client::Client::running() {
                client.plot(
                    tracy_client::plot_name!("compression_ratio"),
                    compressed.len() as f64 / body.len().max(1) as f64,
                );
            }

            compressed
        }
    };

    // What actually has to fit the MTU, so this is the number to watch alongside
    // `datagram_fragments`.
    #[cfg(feature = "tracy")]
    if let Some(client) = tracy_client::Client::running() {
        client.plot(
            tracy_client::plot_name!("datagram_body_bytes"),
            out.len() as f64,
        );
    }

    Ok(out)
}

pub fn decompress(body: Vec<u8>, level: Option<i32>) -> DeformResult<Vec<u8>> {
    if level.is_none() {
        return Ok(body);
    }

    zstd::stream::decode_all(body.as_slice())
        .map_err(|e| DeformError::Deserialize(format!("decompress datagram: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of [`ENVELOPE_OVERHEAD`]: if wincode's integer or length
    /// encoding ever changes, this fails rather than datagrams silently growing past
    /// the MTU and quinn refusing to send them.
    #[test]
    fn envelope_overhead_is_covered() {
        for data_len in [0usize, 1, 127, 128, 255, 256, 1024, 1200, 65535] {
            let datagram = DeformDatagram {
                message_id: u64::MAX,
                fragment_id: u8::MAX,
                total_fragments: u8::MAX,
                data: vec![0u8; data_len],
            };
            let overhead = wincode::serialize(&datagram).unwrap().len() - data_len;
            assert!(
                overhead <= ENVELOPE_OVERHEAD,
                "{data_len} bytes of data needs {overhead} bytes of envelope, over the \
                 reserved {ENVELOPE_OVERHEAD}"
            );
        }
    }

    /// The default `DeformQuicLogic::MAX_FRAGMENTS`, since the tests drive the limit
    /// directly rather than through a `Q`.
    const MAX_FRAGMENTS: u8 = 64;

    /// Whatever the envelope costs, no datagram may exceed the MTU it was cut for.
    #[test]
    fn datagrams_never_exceed_the_mtu() {
        for mtu in [ENVELOPE_OVERHEAD + 1, 64, 512, 1200] {
            let body = vec![0xCDu8; 4000];
            // Tiny MTUs blow the fragment cap, which is its own error path.
            let Ok(datagrams) = split_into_datagrams(1, &body, mtu, MAX_FRAGMENTS) else {
                continue;
            };
            for datagram in datagrams {
                assert!(datagram.len() <= mtu, "{} > {mtu}", datagram.len());
            }
        }
    }

    #[test]
    fn oversized_messages_are_refused() {
        let body = vec![0u8; MAX_FRAGMENTS as usize * 1200];
        assert!(split_into_datagrams(0, &body, 1200, MAX_FRAGMENTS).is_err());

        // An MTU with no room past the envelope must not divide by zero.
        assert!(split_into_datagrams(0, &[], ENVELOPE_OVERHEAD, MAX_FRAGMENTS).is_err());
        assert!(split_into_datagrams(0, &[1, 2, 3], ENVELOPE_OVERHEAD + 1, MAX_FRAGMENTS).is_ok());
    }

    #[test]
    fn compression_roundtrips_and_is_a_noop_when_disabled() {
        let body: Vec<u8> = (0..4000u32).map(|i| (i / 16) as u8).collect();

        let compressed = compress(body.clone(), Some(3)).unwrap();
        assert!(
            compressed.len() < body.len(),
            "repetitive data should shrink"
        );
        assert_eq!(decompress(compressed, Some(3)).unwrap(), body);

        assert_eq!(compress(body.clone(), None).unwrap(), body);
        assert_eq!(decompress(body.clone(), None).unwrap(), body);
    }
}
