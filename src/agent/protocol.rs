use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};

use crate::error::{Result, StrawsError};

/// Operation codes for the binary protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpCode {
    Read = 0,
    Write = 1,
    Md5 = 2,
    Mkdir = 3,
    Stat = 4,
    Truncate = 5,
    Find = 6,
}

impl TryFrom<u8> for OpCode {
    type Error = StrawsError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(OpCode::Read),
            1 => Ok(OpCode::Write),
            2 => Ok(OpCode::Md5),
            3 => Ok(OpCode::Mkdir),
            4 => Ok(OpCode::Stat),
            5 => Ok(OpCode::Truncate),
            6 => Ok(OpCode::Find),
            _ => Err(StrawsError::Protocol(format!("Invalid opcode: {}", value))),
        }
    }
}

/// Request to send to the Python agent
#[derive(Debug, Clone)]
pub struct Request {
    pub op: OpCode,
    pub path: String,
    pub offset: u64,
    pub length: u64,
    pub data: Option<Vec<u8>>, // For write operations
}

impl Request {
    pub fn read(path: impl Into<String>, offset: u64, length: u64) -> Self {
        Request {
            op: OpCode::Read,
            path: path.into(),
            offset,
            length,
            data: None,
        }
    }

    pub fn write(path: impl Into<String>, offset: u64, data: Vec<u8>) -> Self {
        let length = data.len() as u64;
        Request {
            op: OpCode::Write,
            path: path.into(),
            offset,
            length,
            data: Some(data),
        }
    }

    pub fn md5(path: impl Into<String>, offset: u64, length: u64) -> Self {
        Request {
            op: OpCode::Md5,
            path: path.into(),
            offset,
            length,
            data: None,
        }
    }

    pub fn mkdir(path: impl Into<String>) -> Self {
        Request {
            op: OpCode::Mkdir,
            path: path.into(),
            offset: 0,
            length: 0,
            data: None,
        }
    }

    pub fn stat(path: impl Into<String>) -> Self {
        Request {
            op: OpCode::Stat,
            path: path.into(),
            offset: 0,
            length: 0,
            data: None,
        }
    }

    pub fn truncate(path: impl Into<String>, size: u64) -> Self {
        Request {
            op: OpCode::Truncate,
            path: path.into(),
            offset: 0,
            length: size,
            data: None,
        }
    }

    pub fn find(path: impl Into<String>, with_md5: bool) -> Self {
        Request {
            op: OpCode::Find,
            path: path.into(),
            offset: if with_md5 { 1 } else { 0 }, // Use offset as flags: 1 = compute MD5
            length: 0,
            data: None,
        }
    }

    /// Encode request to bytes for sending to agent
    /// Format: op(1) + path_len(2) + path + offset(8) + length(8) [+ data for writes]
    pub fn encode(&self) -> Vec<u8> {
        let path_bytes = self.path.as_bytes();
        let path_len = path_bytes.len();

        let data_len = self.data.as_ref().map(|d| d.len()).unwrap_or(0);
        let mut buf = Vec::with_capacity(1 + 2 + path_len + 8 + 8 + data_len);

        buf.write_u8(self.op as u8).unwrap();
        buf.write_u16::<BigEndian>(path_len as u16).unwrap();
        buf.extend_from_slice(path_bytes);
        buf.write_u64::<BigEndian>(self.offset).unwrap();
        buf.write_u64::<BigEndian>(self.length).unwrap();

        if let Some(ref data) = self.data {
            buf.extend_from_slice(data);
        }

        buf
    }
}

/// Response status from the Python agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Success = 0,
    Error = 1,
}

impl TryFrom<u8> for ResponseStatus {
    type Error = StrawsError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(ResponseStatus::Success),
            1 => Ok(ResponseStatus::Error),
            _ => Err(StrawsError::Protocol(format!(
                "Invalid response status: {}",
                value
            ))),
        }
    }
}

/// Response header from the Python agent
/// Format: status(1) + data_len(8)
#[derive(Debug, Clone)]
pub struct ResponseHeader {
    pub status: ResponseStatus,
    pub data_len: u64,
}

impl ResponseHeader {
    pub const SIZE: usize = 9; // 1 + 8

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::SIZE {
            return Err(StrawsError::Protocol(format!(
                "Response header too short: {} bytes",
                buf.len()
            )));
        }

        let status = ResponseStatus::try_from(buf[0])?;
        let data_len = (&buf[1..9]).read_u64::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read data length: {}", e))
        })?;

        Ok(ResponseHeader { status, data_len })
    }
}

/// Full response (for small responses that fit in memory)
#[derive(Debug, Clone)]
pub struct Response {
    pub status: ResponseStatus,
    pub data: Vec<u8>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        self.status == ResponseStatus::Success
    }

    pub fn error_message(&self) -> Option<String> {
        if self.status == ResponseStatus::Error {
            Some(String::from_utf8_lossy(&self.data).to_string())
        } else {
            None
        }
    }

    /// Parse stat response: size(8) + mode(4) + mtime(8)
    pub fn parse_stat(&self) -> Result<StatInfo> {
        if self.data.len() < 20 {
            return Err(StrawsError::Protocol(format!(
                "Stat response too short: {} bytes",
                self.data.len()
            )));
        }

        let mut cursor = &self.data[..];
        let size = cursor.read_u64::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read size: {}", e))
        })?;
        let mode = cursor.read_u32::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read mode: {}", e))
        })?;
        let mtime = cursor.read_u64::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read mtime: {}", e))
        })?;

        Ok(StatInfo { size, mode, mtime })
    }
}

/// File stat information
#[derive(Debug, Clone)]
pub struct StatInfo {
    pub size: u64,
    pub mode: u32,
    pub mtime: u64,
}

/// Entry from FIND operation
#[derive(Debug, Clone)]
pub struct FindEntry {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: u64,
    pub md5: Option<String>, // Present when find was called with_md5=true
}

impl FindEntry {
    /// Parse a single find entry from bytes
    /// Format: path_len(2) + path(utf8) + size(8) + mode(4) + mtime(8)
    /// Returns None if path_len is 0 (end marker)
    pub fn parse(buf: &[u8]) -> Result<(Option<Self>, usize)> {
        if buf.len() < 2 {
            return Err(StrawsError::Protocol("Find entry buffer too short".into()));
        }

        let path_len = (&buf[0..2]).read_u16::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read path length: {}", e))
        })? as usize;

        // End marker
        if path_len == 0 {
            return Ok((None, 2));
        }

        let needed = 2 + path_len + 8 + 4 + 8; // path_len + path + size + mode + mtime
        if buf.len() < needed {
            return Err(StrawsError::Protocol(format!(
                "Find entry too short: need {} bytes, have {}",
                needed,
                buf.len()
            )));
        }

        let path = String::from_utf8_lossy(&buf[2..2 + path_len]).to_string();
        let mut cursor = &buf[2 + path_len..];

        let size = cursor.read_u64::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read size: {}", e))
        })?;
        let mode = cursor.read_u32::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read mode: {}", e))
        })?;
        let mtime = cursor.read_u64::<BigEndian>().map_err(|e| {
            StrawsError::Protocol(format!("Failed to read mtime: {}", e))
        })?;

        Ok((Some(FindEntry { path, size, mode, mtime, md5: None }), needed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_encode_read() {
        let req = Request::read("/test/path", 1024, 4096);
        let encoded = req.encode();

        assert_eq!(encoded[0], OpCode::Read as u8);
        // path_len = 10
        assert_eq!(encoded[1], 0);
        assert_eq!(encoded[2], 10);
        // path
        assert_eq!(&encoded[3..13], b"/test/path");
        // offset = 1024 (big endian)
        assert_eq!(&encoded[13..21], &[0, 0, 0, 0, 0, 0, 4, 0]);
        // length = 4096 (big endian)
        assert_eq!(&encoded[21..29], &[0, 0, 0, 0, 0, 0, 16, 0]);
    }

    #[test]
    fn test_response_header_decode() {
        let buf = [0u8, 0, 0, 0, 0, 0, 0, 0, 100]; // status=0, len=100
        let header = ResponseHeader::decode(&buf).unwrap();
        assert_eq!(header.status, ResponseStatus::Success);
        assert_eq!(header.data_len, 100);
    }
}
