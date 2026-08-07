//! 协议序列化：请求与响应的帧编码 / 解码（字节序为大端）。

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::ScaffoldingError;
use crate::models::protocol::{ProtocolRequest, ProtocolResponse};

/// 将字符串编码为 ASCII 字节：非 ASCII 字符替换为 `?`（与 C# Encoding.ASCII 语义一致）。
fn ascii_bytes(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect()
}

/// 将请求序列化为帧：`[1B typeLen][type ASCII][4B BE bodyLen][body]`。
pub fn serialize_request(req: &ProtocolRequest) -> Vec<u8> {
    let type_bytes = ascii_bytes(&format!("{}:{}", req.namespace, req.request_type));
    let mut buf = Vec::with_capacity(1 + type_bytes.len() + 4 + req.body.len());
    buf.push(type_bytes.len() as u8);
    buf.extend_from_slice(&type_bytes);
    buf.extend_from_slice(&(req.body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.body);
    buf
}

/// 将响应序列化为帧：`[1B status][4B BE bodyLen][body]`。
pub fn serialize_response(resp: &ProtocolResponse) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5 + resp.body.len());
    buf.push(resp.status);
    buf.extend_from_slice(&(resp.body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&resp.body);
    buf
}

/// 从完整缓冲按游标读取指定字节数；剩余不足时返回"流提前结束"错误。
fn read_exact(bytes: &[u8], offset: &mut usize, count: usize) -> Result<Vec<u8>, ScaffoldingError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let remaining = bytes.len().saturating_sub(*offset);
    if remaining < count {
        return Err(ScaffoldingError::Protocol(format!(
            "流提前结束: 期望读取 {count} 字节，实际读取 {remaining} 字节"
        )));
    }
    let slice = bytes[*offset..*offset + count].to_vec();
    *offset += count;
    Ok(slice)
}

/// 拆分类型字符串为命名空间与请求类型；不含 `:` 时返回"无效的请求类型格式"错误。
fn split_type_str(type_str: &str) -> Result<(String, String), ScaffoldingError> {
    match type_str.find(':') {
        Some(colon) => Ok((
            type_str[..colon].to_string(),
            type_str[colon + 1..].to_string(),
        )),
        None => Err(ScaffoldingError::Protocol(format!(
            "无效的请求类型格式: {type_str}"
        ))),
    }
}

/// 从一次性完整缓冲解析请求帧。
pub fn parse_request(bytes: &[u8]) -> Result<ProtocolRequest, ScaffoldingError> {
    let mut offset = 0usize;
    let type_len = read_exact(bytes, &mut offset, 1)?[0] as usize;
    let type_str =
        String::from_utf8_lossy(&read_exact(bytes, &mut offset, type_len)?).into_owned();
    let len_buf = read_exact(bytes, &mut offset, 4)?;
    let body_len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
    let body = read_exact(bytes, &mut offset, body_len)?;
    let (namespace, request_type) = split_type_str(&type_str)?;
    Ok(ProtocolRequest {
        namespace,
        request_type,
        body,
    })
}

/// 从一次性完整缓冲解析响应帧。
pub fn parse_response(bytes: &[u8]) -> Result<ProtocolResponse, ScaffoldingError> {
    let mut offset = 0usize;
    let status = read_exact(bytes, &mut offset, 1)?[0];
    let len_buf = read_exact(bytes, &mut offset, 4)?;
    let body_len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
    let body = read_exact(bytes, &mut offset, body_len)?;
    Ok(ProtocolResponse { status, body })
}

/// 从异步流完整读取指定字节数（ReadExact）；流提前结束或读取失败时返回协议错误。
async fn read_exact_async<R: AsyncRead + Unpin>(
    r: &mut R,
    count: usize,
) -> Result<Vec<u8>, ScaffoldingError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; count];
    r.read_exact(&mut buf).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ScaffoldingError::Protocol(format!("流提前结束: 期望读取 {count} 字节"))
        } else {
            ScaffoldingError::Protocol(format!("读取流失败: {e}"))
        }
    })?;
    Ok(buf)
}

/// 从异步流按完整流语义解析请求帧。
pub async fn deserialize_request_async<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<ProtocolRequest, ScaffoldingError> {
    let type_len = read_exact_async(r, 1).await?[0] as usize;
    let type_str = String::from_utf8_lossy(&read_exact_async(r, type_len).await?).into_owned();
    let len_buf = read_exact_async(r, 4).await?;
    let body_len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
    let body = read_exact_async(r, body_len).await?;
    let (namespace, request_type) = split_type_str(&type_str)?;
    Ok(ProtocolRequest {
        namespace,
        request_type,
        body,
    })
}

/// 从异步流按完整流语义解析响应帧。
pub async fn deserialize_response_async<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<ProtocolResponse, ScaffoldingError> {
    let status = read_exact_async(r, 1).await?[0];
    let len_buf = read_exact_async(r, 4).await?;
    let body_len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
    let body = read_exact_async(r, body_len).await?;
    Ok(ProtocolResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(namespace: &str, request_type: &str, body: Vec<u8>) -> ProtocolRequest {
        ProtocolRequest {
            namespace: namespace.to_string(),
            request_type: request_type.to_string(),
            body,
        }
    }

    fn response(status: u8, body: Vec<u8>) -> ProtocolResponse {
        ProtocolResponse { status, body }
    }

    #[test]
    fn roundtrip_request_preserves_data() {
        let req = request("c", "ping", vec![0x01, 0x02, 0x03]);
        let bytes = serialize_request(&req);

        let parsed = parse_request(&bytes).expect("请求帧应解析成功");

        assert_eq!(parsed.namespace, "c");
        assert_eq!(parsed.request_type, "ping");
        assert_eq!(parsed.body, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn roundtrip_response_preserves_data() {
        let resp = response(0, vec![0xFF, 0xFE]);
        let bytes = serialize_response(&resp);

        let parsed = parse_response(&bytes).expect("响应帧应解析成功");

        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.body, vec![0xFF, 0xFE]);
    }

    #[test]
    fn roundtrip_response_empty_body_works() {
        let resp = response(0, Vec::new());
        let bytes = serialize_response(&resp);

        let parsed = parse_response(&bytes).expect("空 body 响应帧应解析成功");

        assert_eq!(parsed.status, 0);
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn long_type_name_works() {
        let req = request("myapp", "very_long_type_name", vec![0x42]);
        let bytes = serialize_request(&req);

        let parsed = parse_request(&bytes).expect("长类型名请求帧应解析成功");

        assert_eq!(parsed.namespace, "myapp");
        assert_eq!(parsed.request_type, "very_long_type_name");
        assert_eq!(parsed.body, vec![0x42]);
    }

    #[test]
    fn serialize_request_frame_matches_csharp_bytes() {
        let req = request("c", "ping", vec![0x01, 0x02, 0x03]);

        let expected = [&[0x06][..], b"c:ping", &[0x00, 0x00, 0x00, 0x03][..], &[0x01, 0x02, 0x03][..]].concat();

        assert_eq!(serialize_request(&req), expected);
    }

    #[test]
    fn serialize_response_frame_matches_csharp_bytes() {
        let resp = response(0, vec![0xFF, 0xFE]);

        let expected = [&[0x00][..], &[0x00, 0x00, 0x00, 0x02][..], &[0xFF, 0xFE][..]].concat();

        assert_eq!(serialize_response(&resp), expected);
    }

    #[test]
    fn parse_request_accepts_csharp_known_bytes() {
        let bytes = [
            0x0A, 0x6D, 0x79, 0x61, 0x70, 0x70, 0x3A, 0x70, 0x69, 0x6E, 0x67, 0x00, 0x00, 0x00,
            0x01, 0x42,
        ];

        let parsed = parse_request(&bytes).expect("C# 已知请求帧应解析成功");

        assert_eq!(parsed.namespace, "myapp");
        assert_eq!(parsed.request_type, "ping");
        assert_eq!(parsed.body, vec![0x42]);
    }

    #[test]
    fn parse_response_accepts_csharp_known_bytes() {
        let bytes = [0x01, 0x00, 0x00, 0x00, 0x02, 0x01, 0x02];

        let parsed = parse_response(&bytes).expect("C# 已知响应帧应解析成功");

        assert_eq!(parsed.status, 1);
        assert_eq!(parsed.body, vec![0x01, 0x02]);
    }
}
