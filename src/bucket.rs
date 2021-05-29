use minidom::Element;
use serde_xml_rs as serde_xml;
use std::collections::HashMap;
use std::mem;

use crate::bucket_ops::{BucketConfiguration, CreateBucketResponse};
use crate::command::{Command, Multipart};
use crate::creds::Credentials;
use crate::region::Region;
use std::str::FromStr;

pub type Query = HashMap<String, String>;

use crate::surf_request::SurfRequest as RequestImpl;
use futures::io::AsyncRead;

use crate::request_trait::Request;
use crate::serde_types::{
    BucketLocationResult, CompleteMultipartUploadData, HeadObjectResult,
    InitiateMultipartUploadResponse, ListBucketResult, ListMultipartUploadsResult, Part,
};
use anyhow::anyhow;
use anyhow::Result;
use http::header::HeaderName;
use http::HeaderMap;

pub const CHUNK_SIZE: usize = 8_388_608; // 8 Mebibytes, min is 5 (5_242_880);

#[derive(Debug, PartialEq)]
pub struct Tag {
    key: String,
    value: String,
}

impl Tag {
    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn value(&self) -> String {
        self.value.to_owned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bucket {
    pub name: String,
    pub region: Region,
    pub credentials: Credentials,
    pub extra_headers: HeaderMap,
    pub extra_query: Query,
    path_style: bool,
}

fn validate_expiry(expiry_secs: u32) -> Result<()> {
    if 604800 < expiry_secs {
        return Err(anyhow!(
            "Max expiration for presigned URLs is one week, or 604.800 seconds, got {} instead",
            expiry_secs
        ));
    }
    Ok(())
}

impl Bucket {

    pub fn presign_get<S: AsRef<str>>(&self, path: S, expiry_secs: u32) -> Result<String> {
        validate_expiry(expiry_secs)?;
        let request = RequestImpl::new(self, path.as_ref(), Command::PresignGet { expiry_secs });
        request.presigned()
    }

    pub fn presign_put<S: AsRef<str>>(
        &self,
        path: S,
        expiry_secs: u32,
        custom_headers: Option<HeaderMap>,
    ) -> Result<String> {
        validate_expiry(expiry_secs)?;
        let request = RequestImpl::new(
            self,
            path.as_ref(),
            Command::PresignPut {
                expiry_secs,
                custom_headers,
            },
        );
        request.presigned()
    }

    pub async fn create(
        name: &str,
        region: Region,
        credentials: Credentials,
        config: BucketConfiguration,
    ) -> Result<CreateBucketResponse> {
        let mut config = config;
        config.set_region(region.clone());
        let command = Command::CreateBucket { config };
        let bucket = Bucket::new(name, region, credentials)?;
        let request = RequestImpl::new(&bucket, "", command);
        let (data, response_code) = request.response_data(false).await?;
        let response_text = std::str::from_utf8(&data)?;
        Ok(CreateBucketResponse {
            bucket,
            response_text: response_text.to_string(),
            response_code,
        })
    }

    pub async fn create_with_path_style(
        name: &str,
        region: Region,
        credentials: Credentials,
        config: BucketConfiguration,
    ) -> Result<CreateBucketResponse> {
        let mut config = config;
        config.set_region(region.clone());
        let command = Command::CreateBucket { config };
        let bucket = Bucket::new_with_path_style(name, region, credentials)?;
        let request = RequestImpl::new(&bucket, "", command);
        let (data, response_code) = request.response_data(false).await?;
        let response_text = std::str::from_utf8(&data)?;
        Ok(CreateBucketResponse {
            bucket,
            response_text: response_text.to_string(),
            response_code,
        })
    }

    pub async fn delete(&self) -> Result<u16> {
        let command = Command::DeleteBucket;
        let request = RequestImpl::new(self, "", command);
        let (_, response_code) = request.response_data(false).await?;
        Ok(response_code)
    }

    pub fn new(name: &str, region: Region, credentials: Credentials) -> Result<Bucket> {
        Ok(Bucket {
            name: name.into(),
            region,
            credentials,
            extra_headers: HeaderMap::new(),
            extra_query: HashMap::new(),
            path_style: false,
        })
    }

    pub fn new_public(name: &str, region: Region) -> Result<Bucket> {
        Ok(Bucket {
            name: name.into(),
            region,
            credentials: Credentials::anonymous()?,
            extra_headers: HeaderMap::new(),
            extra_query: HashMap::new(),
            path_style: false,
        })
    }

    pub fn new_with_path_style(
        name: &str,
        region: Region,
        credentials: Credentials,
    ) -> Result<Bucket> {
        Ok(Bucket {
            name: name.into(),
            region,
            credentials,
            extra_headers: HeaderMap::new(),
            extra_query: HashMap::new(),
            path_style: true,
        })
    }

    pub fn new_public_with_path_style(name: &str, region: Region) -> Result<Bucket> {
        Ok(Bucket {
            name: name.into(),
            region,
            credentials: Credentials::anonymous()?,
            extra_headers: HeaderMap::new(),
            extra_query: HashMap::new(),
            path_style: true,
        })
    }

    pub async fn get_object<S: AsRef<str>>(&self, path: S) -> Result<(Vec<u8>, u16)> {
        let command = Command::GetObject;
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(false).await
    }

    pub async fn get_object_torrent<S: AsRef<str>>(&self, path: S) -> Result<(Vec<u8>, u16)> {
        let command = Command::GetObjectTorrent;
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(false).await
    }

    pub async fn get_object_range<S: AsRef<str>>(
        &self,
        path: S,
        start: u64,
        end: Option<u64>,
    ) -> Result<(Vec<u8>, u16)> {
        if let Some(end) = end {
            assert!(start < end);
        }

        let command = Command::GetObjectRange { start, end };
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(false).await
    }

    pub async fn get_object_stream<T: std::io::Write + Send, S: AsRef<str>>(
        &self,
        path: S,
        writer: &mut T,
    ) -> Result<u16> {
        let command = Command::GetObject;
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data_to_writer(writer).await
    }

    pub async fn put_object_stream<R: AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        s3_path: impl AsRef<str>,
    ) -> Result<u16> {
        self._put_object_stream(reader, s3_path.as_ref()).await
    }

    async fn _put_object_stream<R: AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        s3_path: &str,
    ) -> Result<u16> {
        let command = Command::InitiateMultipartUpload;
        let request = RequestImpl::new(self, &s3_path, command);
        let (data, code) = request.response_data(false).await?;
        let msg: InitiateMultipartUploadResponse =
            serde_xml::from_str(std::str::from_utf8(data.as_slice())?)?;
        let path = msg.key;
        let upload_id = &msg.upload_id;

        let mut part_number: u32 = 0;
        let mut etags = Vec::new();
        loop {
            let chunk = crate::utils::read_chunk(reader).await?;

            if chunk.len() < CHUNK_SIZE {
                if part_number == 0 {
                    // Files is not big enough for multipart upload, going with regular put_object
                    self.abort_upload(&path, upload_id).await?;

                    self.put_object(s3_path, chunk.as_slice()).await?;
                    break;
                } else {
                    part_number += 1;
                    let command = Command::PutObject {
                        // part_number,
                        content: &chunk,
                        content_type: "application/octet-stream",
                        multipart: Some(Multipart::new(part_number, upload_id)), // upload_id: &msg.upload_id,
                    };
                    let request = RequestImpl::new(self, &path, command);
                    let (data, _code) = request.response_data(true).await?;
                    let etag = std::str::from_utf8(data.as_slice())?;
                    etags.push(etag.to_string());
                    let inner_data = etags
                        .clone()
                        .into_iter()
                        .enumerate()
                        .map(|(i, x)| Part {
                            etag: x,
                            part_number: i as u32 + 1,
                        })
                        .collect::<Vec<Part>>();
                    let data = CompleteMultipartUploadData { parts: inner_data };
                    let complete = Command::CompleteMultipartUpload {
                        upload_id: &msg.upload_id,
                        data,
                    };
                    let complete_request = RequestImpl::new(self, &path, complete);
                    let (_data, _code) = complete_request.response_data(false).await?;
                    // let response = std::str::from_utf8(data.as_slice())?;
                    break;
                }
            } else {
                part_number += 1;
                let command = Command::PutObject {
                    // part_number,
                    content: &chunk,
                    content_type: "application/octet-stream",
                    multipart: Some(Multipart::new(part_number, upload_id)), // upload_id: &msg.upload_id,
                };
                let request = RequestImpl::new(self, &path, command);
                let (data, _code) = request.response_data(true).await?;
                let etag = std::str::from_utf8(data.as_slice())?;
                etags.push(etag.to_string());
            }
        }
        Ok(code)
    }

    pub async fn location(&self) -> Result<(Region, u16)> {
        let request = RequestImpl::new(self, "?location", Command::GetBucketLocation);
        let result = request.response_data(false).await?;
        let region_string = String::from_utf8_lossy(&result.0);
        let region = match serde_xml::from_reader(region_string.as_bytes()) {
            Ok(r) => {
                let location_result: BucketLocationResult = r;
                location_result.region.parse()?
            }
            Err(e) => {
                if result.1 == 200 {
                    Region::Custom {
                        region: "Custom".to_string(),
                        endpoint: "".to_string(),
                    }
                } else {
                    Region::Custom {
                        region: format!("Error encountered : {}", e),
                        endpoint: "".to_string(),
                    }
                }
            }
        };
        Ok((region, result.1))
    }

    pub async fn delete_object<S: AsRef<str>>(&self, path: S) -> Result<(Vec<u8>, u16)> {
        let command = Command::DeleteObject;
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(false).await
    }

    pub async fn head_object<S: AsRef<str>>(&self, path: S) -> Result<(HeadObjectResult, u16)> {
        let command = Command::HeadObject;
        let request = RequestImpl::new(self, path.as_ref(), command);
        let (headers, status) = request.response_header().await?;
        let header_object = HeadObjectResult::from(&headers);
        Ok((header_object, status))
    }

    pub async fn put_object_with_content_type<S: AsRef<str>>(
        &self,
        path: S,
        content: &[u8],
        content_type: &str,
    ) -> Result<(Vec<u8>, u16)> {
        let command = Command::PutObject {
            content,
            content_type,
            multipart: None,
        };
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(true).await
    }

    pub async fn put_object<S: AsRef<str>>(
        &self,
        path: S,
        content: &[u8],
    ) -> Result<(Vec<u8>, u16)> {
        self.put_object_with_content_type(path, content, "application/octet-stream")
            .await
    }

    fn _tags_xml<S: AsRef<str>>(&self, tags: &[(S, S)]) -> String {
        let mut s = String::new();
        let content = tags
            .iter()
            .map(|&(ref name, ref value)| {
                format!(
                    "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                    name.as_ref(),
                    value.as_ref()
                )
            })
            .fold(String::new(), |mut a, b| {
                a.push_str(b.as_str());
                a
            });
        s.push_str("<Tagging><TagSet>");
        s.push_str(&content);
        s.push_str("</TagSet></Tagging>");
        s
    }

    pub async fn put_object_tagging<S: AsRef<str>>(
        &self,
        path: &str,
        tags: &[(S, S)],
    ) -> Result<(Vec<u8>, u16)> {
        let content = self._tags_xml(&tags);
        let command = Command::PutObjectTagging { tags: &content };
        let request = RequestImpl::new(self, path, command);
        request.response_data(false).await
    }

    pub async fn delete_object_tagging<S: AsRef<str>>(&self, path: S) -> Result<(Vec<u8>, u16)> {
        let command = Command::DeleteObjectTagging;
        let request = RequestImpl::new(self, path.as_ref(), command);
        request.response_data(false).await
    }

    pub async fn get_object_tagging<S: AsRef<str>>(&self, path: S) -> Result<(Vec<Tag>, u16)> {
        let command = Command::GetObjectTagging {};
        let request = RequestImpl::new(self, path.as_ref(), command);
        let result = request.response_data(false).await?;

        let mut tags = Vec::new();

        if result.1 == 200 {
            let result_string = String::from_utf8_lossy(&result.0);
            let ns = "http://s3.amazonaws.com/doc/2006-03-01/";
            if let Ok(tagging) = result_string.parse::<Element>() {
                for tag_set in tagging.children() {
                    if tag_set.is("TagSet", ns) {
                        for tag in tag_set.children() {
                            if tag.is("Tag", ns) {
                                let key = if let Some(element) = tag.get_child("Key", ns) {
                                    element.text()
                                } else {
                                    "Could not parse Key from Tag".to_string()
                                };
                                let value = if let Some(element) = tag.get_child("Values", ns) {
                                    element.text()
                                } else {
                                    "Could not parse Values from Tag".to_string()
                                };
                                tags.push(Tag { key, value });
                            }
                        }
                    }
                }
            }
        }

        Ok((tags, result.1))
    }

    pub async fn list_page(
        &self,
        prefix: String,
        delimiter: Option<String>,
        continuation_token: Option<String>,
        start_after: Option<String>,
        max_keys: Option<usize>,
    ) -> Result<(ListBucketResult, u16)> {
        let command = Command::ListBucket {
            prefix,
            delimiter,
            continuation_token,
            start_after,
            max_keys,
        };
        let request = RequestImpl::new(self, "/", command);
        let (response, status_code) = request.response_data(false).await?;
        return serde_xml::from_reader(response.as_slice())
            .map(|list_bucket_result| (list_bucket_result, status_code))
            .map_err(|e| anyhow!("Could not deserialize result \n {}", e));
    }

    pub async fn list(
        &self,
        prefix: String,
        delimiter: Option<String>,
    ) -> Result<Vec<ListBucketResult>> {
        let the_bucket = self.to_owned();
        let mut results = Vec::new();
        let mut continuation_token = None;

        loop {
            let (list_bucket_result, _) = the_bucket
                .list_page(
                    prefix.clone(),
                    delimiter.clone(),
                    continuation_token,
                    None,
                    None,
                )
                .await?;
            continuation_token = list_bucket_result.next_continuation_token.clone();
            results.push(list_bucket_result);
            if continuation_token.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn list_multiparts_uploads_page(
        &self,
        prefix: Option<&str>,
        delimiter: Option<&str>,
        key_marker: Option<String>,
        max_uploads: Option<usize>,
    ) -> Result<(ListMultipartUploadsResult, u16)> {
        let command = Command::ListMultipartUploads {
            prefix,
            delimiter,
            key_marker,
            max_uploads,
        };
        let request = RequestImpl::new(self, "/", command);
        let (response, status_code) = request.response_data(false).await?;
        return serde_xml::from_reader(response.as_slice())
            .map(|list_bucket_result| (list_bucket_result, status_code))
            .map_err(|e| anyhow!("Could not deserialize result \n {}", e));
    }

    pub async fn list_multiparts_uploads(
        &self,
        prefix: Option<&str>,
        delimiter: Option<&str>,
    ) -> Result<Vec<ListMultipartUploadsResult>> {
        let the_bucket = self.to_owned();
        let mut results = Vec::new();
        let mut next_marker: Option<String> = None;

        loop {
            let (list_multiparts_uploads_result, _) = the_bucket
                .list_multiparts_uploads_page(prefix, delimiter, next_marker, None)
                .await?;

            let is_truncated = list_multiparts_uploads_result.is_truncated;
            next_marker = list_multiparts_uploads_result.next_marker.clone();
            results.push(list_multiparts_uploads_result);

            if !is_truncated {
                break;
            }
        }

        Ok(results)
    }

    pub async fn abort_upload(&self, key: &str, upload_id: &str) -> Result<()> {
        let abort = Command::AbortMultipartUpload { upload_id };
        let abort_request = RequestImpl::new(self, key, abort);
        let (content, code) = abort_request.response_data(false).await?;

        if (200..300).contains(&code) {
            Ok(())
        } else {
            let utf8_content = String::from_utf8(content);
            let err = if let Ok(utf8_content) = utf8_content {
                format!(
                    "Invalid return code: got HTTP {} with content '{}'",
                    code, utf8_content
                )
            } else {
                format!(
                    "Invalid return code: got HTTP {} with invalid UTF8 content",
                    code
                )
            };
            Err(anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err,
            )))
        }
    }

    /// Get path_style field of the Bucket struct
    pub fn is_path_style(&self) -> bool {
        self.path_style
    }

    // Get negated path_style field of the Bucket struct
    pub fn is_subdomain_style(&self) -> bool {
        !self.path_style
    }

    /// Configure bucket to use path-style urls and headers
    pub fn set_path_style(&mut self) {
        self.path_style = true;
    }

    /// Configure bucket to use subdomain style urls and headers \[default\]
    pub fn set_subdomain_style(&mut self) {
        self.path_style = false;
    }

    /// Get a reference to the name of the S3 bucket.
    pub fn name(&self) -> String {
        self.name.to_string()
    }

    // Get a reference to the hostname of the S3 API endpoint.
    pub fn host(&self) -> String {
        if self.path_style {
            self.path_style_host()
        } else {
            self.subdomain_style_host()
        }
    }

    pub fn url(&self) -> String {
        if self.path_style {
            format!(
                "{}://{}/{}",
                self.scheme(),
                self.path_style_host(),
                self.name()
            )
        } else {
            format!("{}://{}", self.scheme(), self.subdomain_style_host())
        }
    }

    /// Get a paths-style reference to the hostname of the S3 API endpoint.
    pub fn path_style_host(&self) -> String {
        self.region.host()
    }

    pub fn subdomain_style_host(&self) -> String {
        format!("{}.{}", self.name, self.region.host())
    }

    // pub fn self_host(&self) -> String {
    //     format!("{}.{}", self.name, self.region.host())
    // }

    pub fn scheme(&self) -> String {
        self.region.scheme()
    }

    /// Get the region this object will connect to.
    pub fn region(&self) -> Region {
        self.region.clone()
    }

    /// Get a reference to the AWS access key.
    pub fn access_key(&self) -> Option<String> {
        if let Some(access_key) = self.credentials.access_key.clone() {
            Some(access_key.replace('\n', ""))
        } else {
            None
        }
    }

    /// Get a reference to the AWS secret key.
    pub fn secret_key(&self) -> Option<String> {
        if let Some(secret_key) = self.credentials.secret_key.clone() {
            Some(secret_key.replace('\n', ""))
        } else {
            None
        }
    }

    /// Get a reference to the AWS security token.
    pub fn security_token(&self) -> Option<&str> {
        self.credentials.security_token.as_deref()
    }

    /// Get a reference to the AWS session token.
    pub fn session_token(&self) -> Option<&str> {
        self.credentials.session_token.as_deref()
    }

    /// Get a reference to the full [`Credentials`](struct.Credentials.html)
    /// object used by this `Bucket`.
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Change the credentials used by the Bucket, returning the existing
    /// credentials.
    pub fn set_credentials(&mut self, credentials: Credentials) -> Credentials {
        mem::replace(&mut self.credentials, credentials)
    }

    /// Add an extra header to send with requests to S3.
    ///
    /// Add an extra header to send with requests. Note that the library
    /// already sets a number of headers - headers set with this method will be
    /// overridden by the library headers:
    ///   * Host
    ///   * Content-Type
    ///   * Date
    ///   * Content-Length
    ///   * Authorization
    ///   * X-Amz-Content-Sha256
    ///   * X-Amz-Date
    pub fn add_header(&mut self, key: &str, value: &str) {
        self.extra_headers
            .insert(HeaderName::from_str(key).unwrap(), value.parse().unwrap());
    }

    /// Get a reference to the extra headers to be passed to the S3 API.
    pub fn extra_headers(&self) -> &HeaderMap {
        &self.extra_headers
    }

    /// Get a mutable reference to the extra headers to be passed to the S3
    /// API.
    pub fn extra_headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.extra_headers
    }

    /// Add an extra query pair to the URL used for S3 API access.
    pub fn add_query(&mut self, key: &str, value: &str) {
        self.extra_query.insert(key.into(), value.into());
    }

    /// Get a reference to the extra query pairs to be passed to the S3 API.
    pub fn extra_query(&self) -> &Query {
        &self.extra_query
    }

    /// Get a mutable reference to the extra query pairs to be passed to the S3
    /// API.
    pub fn extra_query_mut(&mut self) -> &mut Query {
        &mut self.extra_query
    }
}
