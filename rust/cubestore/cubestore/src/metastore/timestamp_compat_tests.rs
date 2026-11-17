//! Timestamps used to be chrono values. These tests pin down that rows persisted in RocksDB and
//! messages exchanged between nodes by builds from that time still load, and that what is
//! written now stays readable for them.
//!
//! The payloads are flexbuffers, hex encoded, exactly as a chrono build serialized them: a
//! plain timestamp is an RFC 3339 string ending in `Z` with 0, 3, 6 or 9 fractional digits, and
//! a cachestore timestamp is an integer number of seconds since the Unix epoch.

use crate::cachestore::{CacheItem, QueueItem, QueueItemPayload, QueueResult};
use crate::metastore::job::Job;
use crate::metastore::replay_handle::ReplayHandle;
use crate::metastore::table::Table;
use crate::metastore::Chunk;
use crate::remotefs::RemoteFile;
use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

const TABLE: &str = concat!(
    "7461626c655f6e616d6500066f726465727300736368656d615f696400636f6c756d6e73006e616d",
    "650002696400636f6c756d6e5f747970650003496e7400636f6c756d6e5f696e64657800030e202a",
    "03010300192a0814140107246c6f636174696f6e7300696d706f72745f666f726d6174006861735f",
    "646174610069735f726561647900637265617465645f6174001e323032332d30352d31375430383a",
    "30393a31302e3132333435363738395a006275696c645f72616e67655f656e640014323032332d30",
    "352d31385430303a30303a30305a007365616c5f6174007365616c65640073656c6563745f737461",
    "74656d656e7400736f757263655f636f6c756d6e730073747265616d5f6f666673657400756e6971",
    "75655f6b65795f636f6c756d6e5f696e6469636573006167677265676174655f636f6c756d6e5f69",
    "6e646963657300007365715f636f6c756d6e5f696e646578006c6f636174696f6e5f646f776e6c6f",
    "61645f73697a657300706172746974696f6e5f73706c69745f7468726573686f6c6400657874656e",
    "73696f6e000014006a00e9007f01180115002e013e0129014f004e013b009b01d900d300ce006e00",
    "c100b400bc01aa002800020014007e00060170013a01000000000000010000000000000001000000",
    "00000000000000000000de01000028142814016901690101010901690101010114013c2501",
);

const CHUNK: &str = concat!(
    "706172746974696f6e5f696400726f775f636f756e740075706c6f6164656400616374697665006c",
    "6173745f7573656400696e5f6d656d6f727900637265617465645f61740018323032332d30352d31",
    "375430383a30393a31302e3130305a006f6c646573745f696e736572745f6174001b323032332d30",
    "352d31375430383a30393a31302e3030303132335a0064656163746976617465645f617400737566",
    "666978000861757272623234790066696c655f73697a65007265706c61795f68616e646c655f6964",
    "006d696e006d6178000eb2984627a5b00b107adb24d041c80e010e009d000000000000790700644a",
    "0068140000680000001408000814681c2401",
);

const JOB: &str = concat!(
    "726f775f7265666572656e6365005461626c65000a506172746974696f6e7300020c051408011801",
    "010109286a6f625f747970650013506172746974696f6e436f6d70616374696f6e006c6173745f68",
    "656172745f626561740014323032332d30352d31375430383a30393a31305a007374617475730053",
    "63686564756c6564000573686172640001120101010b140464479223040104613d6f0d1414242408",
    "2401",
);

const REPLAY_HANDLE: &str = concat!(
    "7461626c655f696400637265617465645f61740018313936392d31322d33315432333a35393a3539",
    "2e3530305a007365715f706f696e746572735f62795f6c6f636174696f6e006861735f6661696c65",
    "645f746f5f706572736973745f6368756e6b7300045c1f39680401045700000314680008082401",
);

const REMOTE_FILE: &str = concat!(
    "72656d6f74655f70617468000b612f622e7061727175657400757064617465640018323032332d30",
    "352d31375430383a30393a31302e3235305a0066696c655f73697a6500030b472f0301032a402c08",
    "1414062401",
);

const CACHE_ITEM: &str = concat!(
    "7072656669780006707265666978006b657900036b65790076616c7565000576616c756500657870",
    "6972650004081f2f18000000070000000100000004000000a68b646430000000400000002d000000",
    "06141414142601",
);

const CACHE_ITEM_NO_TTL: &str = concat!(
    "7072656669780006707265666978006b657900036b65790076616c7565000576616c756500657870",
    "6972650004081f2f1804010400212e1800141414082401",
);

const QUEUE_ITEM: &str = concat!(
    "7072656669780006707265666978006b657900036b65790065787472610073746174757300064163",
    "74697665007072696f72697479006372656174656400686561727462656174006f727068616e6564",
    "006578706972650070726f636573735f6964000470726f63000a341a542f5f2770441a550a000000",
    "010000000a000000a68b6464c6a7646400000000b08b64647c000000000000009000000005000000",
    "3c0000007e00000006060206140214061414322601",
);

const QUEUE_RESULT: &str = concat!(
    "706174680004706174680076616c7565000576616c75650064656c65746564006578706972650004",
    "10092a2004000000010000000400000000000000a68b64643a000000320000006a061414142601",
);

const QUEUE_ITEM_PAYLOAD: &str = concat!(
    "76616c756500077061796c6f61640063726561746564006578706972650003100921000005000000",
    "0100000003000000a68b6464c6a76464310000000606140f2601",
);

fn to_flexbuffer<T: Serialize>(value: &T) -> Vec<u8> {
    let mut serializer = flexbuffers::FlexbufferSerializer::new();
    value.serialize(&mut serializer).unwrap();
    serializer.take_buffer()
}

fn from_flexbuffer<T: DeserializeOwned>(bytes: &[u8]) -> T {
    T::deserialize(flexbuffers::Reader::get_root(bytes).unwrap()).unwrap()
}

/// Loads a row a chrono build wrote, and checks that it survives being written back.
fn read<T: Serialize + DeserializeOwned + Debug>(hex_payload: &str) -> T {
    let row: T = from_flexbuffer(&hex::decode(hex_payload).unwrap());
    let rewritten: T = from_flexbuffer(&to_flexbuffer(&row));
    assert_eq!(format!("{:?}", rewritten), format!("{:?}", row));
    row
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

#[test]
fn reads_metastore_rows_written_with_chrono() {
    let table: Table = read(TABLE);
    assert_eq!(
        *table.created_at(),
        Some(ts("2023-05-17T08:09:10.123456789Z"))
    );
    assert_eq!(*table.build_range_end(), Some(ts("2023-05-18T00:00:00Z")));
    assert_eq!(*table.seal_at(), None);

    let chunk: Chunk = read(CHUNK);
    assert_eq!(*chunk.created_at(), Some(ts("2023-05-17T08:09:10.1Z")));
    assert_eq!(
        *chunk.oldest_insert_at(),
        Some(ts("2023-05-17T08:09:10.000123Z"))
    );
    assert_eq!(*chunk.deactivated_at(), None);

    let job: Job = read(JOB);
    assert_eq!(*job.last_heart_beat(), ts("2023-05-17T08:09:10Z"));

    let replay_handle: ReplayHandle = read(REPLAY_HANDLE);
    assert_eq!(*replay_handle.created_at(), ts("1969-12-31T23:59:59.5Z"));

    let remote_file: RemoteFile = read(REMOTE_FILE);
    assert_eq!(*remote_file.updated(), ts("2023-05-17T08:09:10.25Z"));
}

#[test]
fn reads_cachestore_rows_written_with_chrono() {
    let cache_item: CacheItem = read(CACHE_ITEM);
    assert_eq!(*cache_item.get_expire(), Some(ts("2023-05-17T08:09:10Z")));

    let cache_item: CacheItem = read(CACHE_ITEM_NO_TTL);
    assert_eq!(*cache_item.get_expire(), None);

    let queue_item: QueueItem = read(QUEUE_ITEM);
    assert_eq!(*queue_item.get_created(), ts("2023-05-17T08:09:10Z"));
    assert_eq!(
        *queue_item.get_heartbeat(),
        Some(ts("2023-05-17T08:09:20Z"))
    );
    assert_eq!(*queue_item.get_orphaned(), None);
    assert_eq!(*queue_item.get_expire(), ts("2023-05-17T10:09:10Z"));

    let queue_result: QueueResult = read(QUEUE_RESULT);
    assert_eq!(*queue_result.get_expire(), ts("2023-05-17T08:09:10Z"));

    let payload: QueueItemPayload = read(QUEUE_ITEM_PAYLOAD);
    assert_eq!(*payload.get_created(), ts("2023-05-17T08:09:10Z"));
    assert_eq!(*payload.get_expire(), ts("2023-05-17T10:09:10Z"));
}

/// A plain timestamp is written as an RFC 3339 string in UTC. The fraction has as few digits as
/// possible, where chrono padded it to 3, 6 or 9; chrono reads any number of digits.
#[test]
fn writes_timestamps_as_rfc3339_strings() {
    let written = |s: &str| {
        let buffer = to_flexbuffer(&ts(s));
        flexbuffers::Reader::get_root(buffer.as_slice())
            .unwrap()
            .as_str()
            .to_string()
    };
    assert_eq!(written("2023-05-17T08:09:10Z"), "2023-05-17T08:09:10Z");
    assert_eq!(
        written("2023-05-17T08:09:10.100Z"),
        "2023-05-17T08:09:10.1Z"
    );
    assert_eq!(
        written("2023-05-17T08:09:10.000123Z"),
        "2023-05-17T08:09:10.000123Z"
    );
    assert_eq!(
        written("2023-05-17T08:09:10.123456789Z"),
        "2023-05-17T08:09:10.123456789Z"
    );
    assert_eq!(
        written("2023-05-17T10:09:10.5+02:00"),
        "2023-05-17T08:09:10.5Z"
    );
}

/// Cachestore rows keep their timestamps as integer seconds. The cachestore compaction filter
/// reads `expire` straight from the serialized row and relies on that.
#[test]
fn writes_cachestore_timestamps_as_seconds() {
    fn field<'a>(buffer: &'a [u8], name: &str) -> flexbuffers::Reader<'a> {
        flexbuffers::Reader::get_root(buffer)
            .unwrap()
            .as_map()
            .idx(name)
    }

    for payload in [CACHE_ITEM, QUEUE_RESULT, QUEUE_ITEM_PAYLOAD] {
        let chrono_written = hex::decode(payload).unwrap();
        assert_eq!(
            field(&chrono_written, "expire").flexbuffer_type(),
            flexbuffers::FlexBufferType::Int
        );
    }

    let cache_item = to_flexbuffer(&read::<CacheItem>(CACHE_ITEM));
    assert_eq!(
        field(&cache_item, "expire").flexbuffer_type(),
        flexbuffers::FlexBufferType::Int
    );
    assert_eq!(field(&cache_item, "expire").as_i64(), 1_684_310_950);

    let cache_item = to_flexbuffer(&read::<CacheItem>(CACHE_ITEM_NO_TTL));
    assert_eq!(
        field(&cache_item, "expire").flexbuffer_type(),
        flexbuffers::FlexBufferType::Null
    );

    let queue_item = to_flexbuffer(&read::<QueueItem>(QUEUE_ITEM));
    assert_eq!(field(&queue_item, "created").as_i64(), 1_684_310_950);
    assert_eq!(field(&queue_item, "heartbeat").as_i64(), 1_684_310_960);
    assert_eq!(
        field(&queue_item, "orphaned").flexbuffer_type(),
        flexbuffers::FlexBufferType::Null
    );
    assert_eq!(field(&queue_item, "expire").as_i64(), 1_684_318_150);

    let queue_result = to_flexbuffer(&read::<QueueResult>(QUEUE_RESULT));
    assert_eq!(field(&queue_result, "expire").as_i64(), 1_684_310_950);

    let payload = to_flexbuffer(&read::<QueueItemPayload>(QUEUE_ITEM_PAYLOAD));
    assert_eq!(field(&payload, "created").as_i64(), 1_684_310_950);
    assert_eq!(field(&payload, "expire").as_i64(), 1_684_318_150);
}
