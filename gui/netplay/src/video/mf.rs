//! Media Foundation's video encoders and decoders, the ones Windows ships and the graphics
//! card's (NVENC and the like), which are asynchronous.
#![allow(unsafe_code)]

use std::{
    io,
    mem::ManuallyDrop,
    ptr,
    sync::OnceLock,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
            CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
            CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
            IMFActivate, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform,
            METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE,
            MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
            MF_EVENT_FLAG_NO_WAIT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
            MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
            MFSTARTUP_FULL, MFStartup, MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
            MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
            MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::VARIANT,
        },
    },
    core::{GUID, Interface},
};

use super::{Codec, Nv12};

/// How long an asynchronous encoder may take to ask for a picture or hand one out.
const WAIT_INPUT: Duration = Duration::from_millis(200);
const WAIT_OUTPUT: Duration = Duration::from_millis(40);
/// Seconds between pictures that need nothing before them, should one get lost.
const KEYFRAME_SECONDS: u32 = 4;

fn error(error: &windows::core::Error) -> io::Error {
    io::Error::other(format!("Media Foundation: {error}"))
}

/// Starts Media Foundation once, and COM on the calling thread.
fn startup() -> io::Result<()> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    // Joining COM's free-threaded apartment fails harmlessly where the thread already has one.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string()))
        .clone()
        .map_err(io::Error::other)
}

const fn subtype(codec: Codec) -> Option<GUID> {
    match codec {
        Codec::H264 => Some(MFVideoFormat_H264),
        Codec::Hevc => Some(MFVideoFormat_HEVC),
        Codec::Jpeg => None,
    }
}

/// The transforms of `category` from `input` to `output`, best first.
fn find(category: GUID, flags: MFT_ENUM_FLAG, input: GUID, output: GUID) -> Vec<IMFActivate> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: input };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: output };
    let mut list: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0;
    unsafe {
        if MFTEnumEx(
            category,
            flags,
            Some(&raw const input),
            Some(&raw const output),
            &raw mut list,
            &raw mut count,
        )
        .is_err()
            || list.is_null()
        {
            return Vec::new();
        }
        let found = std::slice::from_raw_parts_mut(list, count as usize)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        CoTaskMemFree(Some(list.cast_const().cast()));
        found
    }
}

fn video_type(
    subtype: GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        media.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))?;
        media.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0.cast_unsigned())?;
        Ok(media)
    }
}

/// A sample holding a copy of `data`.
fn sample_of(data: &[u8], time: Duration) -> windows::core::Result<IMFSample> {
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    unsafe {
        let buffer = MFCreateMemoryBuffer(length)?;
        let mut target = ptr::null_mut();
        buffer.Lock(&raw mut target, None, None)?;
        ptr::copy_nonoverlapping(data.as_ptr(), target, length as usize);
        buffer.Unlock()?;
        buffer.SetCurrentLength(length)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        let ticks = i64::try_from(time.as_nanos() / 100).unwrap_or(i64::MAX);
        sample.SetSampleTime(ticks)?;
        sample.SetSampleDuration(166_667)?;
        Ok(sample)
    }
}

/// Takes one finished sample out of `transform`. `Err` carries `MF_E_TRANSFORM_NEED_MORE_INPUT`
/// and `MF_E_TRANSFORM_STREAM_CHANGE` for the caller to act on.
fn take_output(transform: &IMFTransform) -> windows::core::Result<Option<IMFSample>> {
    unsafe {
        let info = transform.GetOutputStreamInfo(0)?;
        let provided = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0.cast_unsigned() != 0;
        let sample = if provided {
            None
        } else {
            let buffer = MFCreateMemoryBuffer(info.cbSize.max(1 << 16))?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            Some(sample)
        };
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = transform.ProcessOutput(0, &mut output, &raw mut status);
        let [output] = output;
        drop(ManuallyDrop::into_inner(output.pEvents));
        let sample = ManuallyDrop::into_inner(output.pSample);
        result.map(|()| sample)
    }
}

/// Appends what `sample` holds to `out`.
fn read_sample(sample: &IMFSample, out: &mut Vec<u8>) -> windows::core::Result<()> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = ptr::null_mut();
        let mut length = 0;
        buffer.Lock(&raw mut data, None, Some(&raw mut length))?;
        out.extend_from_slice(std::slice::from_raw_parts(data, length as usize));
        buffer.Unlock()
    }
}

pub struct Encoder {
    transform: IMFTransform,
    /// Set for asynchronous (hardware) encoders, which say when they want input.
    events: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    /// Pictures the encoder asked for and did not get yet.
    wanted: u32,
}

// Media Foundation's transforms live in the free-threaded apartment.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(codec: Codec, width: u32, height: u32, fps: u32, bitrate: u32) -> io::Result<Self> {
        startup()?;
        let output = subtype(codec).ok_or_else(|| io::Error::other("not a video codec"))?;
        let mut last = io::Error::new(io::ErrorKind::Unsupported, format!("no {codec:?} encoder"));
        for flags in [
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        ] {
            for activate in find(MFT_CATEGORY_VIDEO_ENCODER, flags, MFVideoFormat_NV12, output) {
                match Self::open(&activate, output, width, height, fps, bitrate) {
                    Ok(encoder) => return Ok(encoder),
                    Err(error) => {
                        log::info!("netplay: {codec:?} encoder refused {width}x{height}: {error}");
                        last = error;
                        let _ = unsafe { activate.ShutdownObject() };
                    }
                }
            }
        }
        Err(last)
    }

    fn open(
        activate: &IMFActivate,
        output: GUID,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> io::Result<Self> {
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().map_err(|e| error(&e))?;
            let attributes = transform.GetAttributes().ok();
            let asynchronous = attributes
                .as_ref()
                .and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok())
                .is_some_and(|value| value != 0);
            if let Some(attributes) = attributes.as_ref().filter(|_| asynchronous) {
                attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(|e| error(&e))?;
            }
            let codec_api = transform.cast::<ICodecAPI>().ok();
            let settings = |api: &ICodecAPI| {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let mode = eAVEncCommonRateControlMode_CBR.0.cast_unsigned();
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(mode));
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate));
                let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0_u32));
                let gop = fps * KEYFRAME_SECONDS;
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(gop));
            };
            if let Some(api) = &codec_api {
                settings(api);
            }
            let out_type = video_type(output, width, height, fps).map_err(|e| error(&e))?;
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(|e| error(&e))?;
            transform.SetOutputType(0, &out_type, 0).map_err(|e| error(&e))?;
            let in_type =
                video_type(MFVideoFormat_NV12, width, height, fps).map_err(|e| error(&e))?;
            transform.SetInputType(0, &in_type, 0).map_err(|e| error(&e))?;
            // Some encoders only take these once they know the sizes.
            if let Some(api) = &codec_api {
                settings(api);
            }
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| error(&e))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| error(&e))?;
            let events = if asynchronous {
                Some(transform.cast::<IMFMediaEventGenerator>().map_err(|e| error(&e))?)
            } else {
                None
            };
            Ok(Self { transform, events, codec_api, wanted: 0 })
        }
    }

    pub fn encode(&mut self, nv12: &[u8], time: Duration, keyframe: bool) -> io::Result<Vec<u8>> {
        if keyframe && let Some(api) = &self.codec_api {
            let _ =
                unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1_u32)) };
        }
        let sample = sample_of(nv12, time).map_err(|e| error(&e))?;
        let mut out = Vec::new();
        if self.events.is_none() {
            unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| error(&e))?;
            self.drain(&mut out)?;
            return Ok(out);
        }
        let deadline = Instant::now() + WAIT_INPUT;
        while self.wanted == 0 {
            if !self.next_event(&mut out, deadline)? {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the encoder wants no input"));
            }
        }
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| error(&e))?;
        self.wanted -= 1;
        let deadline = Instant::now() + WAIT_OUTPUT;
        while out.is_empty() && self.next_event(&mut out, deadline)? {}
        // Requests for the next picture that are already there.
        while self.next_event(&mut out, Instant::now())? {}
        Ok(out)
    }

    /// Handles one event of an asynchronous encoder, waiting until `deadline` for it.
    fn next_event(&mut self, out: &mut Vec<u8>, deadline: Instant) -> io::Result<bool> {
        let Some(events) = &self.events else { return Ok(false) };
        loop {
            match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    let kind = unsafe { event.GetType() }.map_err(|e| error(&e))?;
                    if kind == METransformNeedInput.0.cast_unsigned() {
                        self.wanted += 1;
                    } else if kind == METransformHaveOutput.0.cast_unsigned() {
                        self.output(out)?;
                    }
                    return Ok(true);
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_micros(250));
                }
                Err(e) => return Err(error(&e)),
            }
        }
    }

    /// Takes everything a synchronous encoder has.
    fn drain(&self, out: &mut Vec<u8>) -> io::Result<()> {
        loop {
            match self.output(out) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn output(&self, out: &mut Vec<u8>) -> io::Result<()> {
        match take_output(&self.transform) {
            Ok(Some(sample)) => read_sample(&sample, out).map_err(|e| error(&e)),
            Ok(None) => Ok(()),
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "needs more input"))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe {
                let media = self.transform.GetOutputAvailableType(0, 0).map_err(|e| error(&e))?;
                self.transform.SetOutputType(0, &media, 0).map_err(|e| error(&e))
            },
            Err(e) => Err(error(&e)),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // Asynchronous transforms hold a reference to themselves until shut down.
        if let Ok(shutdown) =
            self.transform.cast::<windows::Win32::Media::MediaFoundation::IMFShutdown>()
        {
            let _ = unsafe { shutdown.Shutdown() };
        }
    }
}

pub struct Decoder {
    transform: IMFTransform,
    /// Size of the decoder's pictures, which round the height up (1080 becomes 1088).
    coded_height: u32,
    stride: u32,
}

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(codec: Codec, width: u32, height: u32) -> io::Result<Self> {
        startup()?;
        let input = subtype(codec).ok_or_else(|| io::Error::other("not a video codec"))?;
        let mut last = io::Error::new(io::ErrorKind::Unsupported, format!("no {codec:?} decoder"));
        let flags = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        for activate in find(MFT_CATEGORY_VIDEO_DECODER, flags, input, MFVideoFormat_NV12) {
            match Self::open(&activate, input, width, height) {
                Ok(decoder) => return Ok(decoder),
                Err(error) => {
                    last = error;
                    let _ = unsafe { activate.ShutdownObject() };
                }
            }
        }
        Err(last)
    }

    fn open(activate: &IMFActivate, input: GUID, width: u32, height: u32) -> io::Result<Self> {
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().map_err(|e| error(&e))?;
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let in_type = video_type(input, width, height, 60).map_err(|e| error(&e))?;
            transform.SetInputType(0, &in_type, 0).map_err(|e| error(&e))?;
            let mut decoder = Self { transform, coded_height: height, stride: width };
            decoder.choose_output()?;
            let transform = &decoder.transform;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| error(&e))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| error(&e))?;
            Ok(decoder)
        }
    }

    /// Asks for NV12, which every decoder gives.
    fn choose_output(&mut self) -> io::Result<()> {
        for index in 0.. {
            let media = unsafe { self.transform.GetOutputAvailableType(0, index) }
                .map_err(|e| error(&e))?;
            if unsafe { media.GetGUID(&MF_MT_SUBTYPE) }.ok() != Some(MFVideoFormat_NV12) {
                continue;
            }
            unsafe { self.transform.SetOutputType(0, &media, 0) }.map_err(|e| error(&e))?;
            if let Ok(size) = unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE) } {
                #[allow(clippy::cast_possible_truncation)]
                let (width, height) = ((size >> 32) as u32, size as u32);
                self.coded_height = height;
                self.stride = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.unwrap_or(width);
            }
            return Ok(());
        }
        Err(io::Error::other("the decoder gives no NV12"))
    }

    pub fn decode(
        &mut self,
        data: &[u8],
        time: Duration,
        width: u32,
        height: u32,
    ) -> io::Result<Option<Nv12>> {
        let sample = sample_of(data, time).map_err(|e| error(&e))?;
        let mut picture = None;
        loop {
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => break,
                // Full: what it holds has to come out first.
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    if let Some(newer) = self.drain(width, height)? {
                        picture = Some(newer);
                    } else {
                        return Err(error(&e));
                    }
                }
                Err(e) => return Err(error(&e)),
            }
        }
        Ok(self.drain(width, height)?.or(picture))
    }

    /// The newest picture the decoder has.
    fn drain(&mut self, width: u32, height: u32) -> io::Result<Option<Nv12>> {
        let mut newest = None;
        loop {
            match take_output(&self.transform) {
                Ok(Some(sample)) => newest = Some(self.picture(&sample, width, height)?),
                Ok(None) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(newest),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                Err(e) => return Err(error(&e)),
            }
        }
    }

    /// Packs the decoder's padded planes into `width` x `height`.
    fn picture(&self, sample: &IMFSample, width: u32, height: u32) -> io::Result<Nv12> {
        let (w, h) = (width as usize, height as usize);
        let mut data = vec![0; Nv12::size(width, height)];
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(|e| error(&e))?;
            let mut base = ptr::null_mut();
            let mut length = 0;
            let mut pitch = self.stride as usize;
            let two_d = buffer.cast::<IMF2DBuffer>().ok();
            if let Some(two_d) = &two_d {
                let mut signed = 0;
                two_d.Lock2D(&raw mut base, &raw mut signed).map_err(|e| error(&e))?;
                pitch = signed.unsigned_abs() as usize;
                length = (pitch * self.coded_height as usize * 3 / 2) as u32;
            } else {
                buffer.Lock(&raw mut base, None, Some(&raw mut length)).map_err(|e| error(&e))?;
            }
            let source = std::slice::from_raw_parts(base, length as usize);
            let chroma_start = pitch * self.coded_height.max(height) as usize;
            let mut copy = |from: usize, to: usize| {
                if let (Some(row), Some(slot)) =
                    (source.get(from..from + w), data.get_mut(to..to + w))
                {
                    slot.copy_from_slice(row);
                }
            };
            for row in 0..h {
                copy(row * pitch, row * w);
            }
            for row in 0..h / 2 {
                copy(chroma_start + row * pitch, w * h + row * w);
            }
            if let Some(two_d) = &two_d {
                let _ = two_d.Unlock2D();
            } else {
                let _ = buffer.Unlock();
            }
        }
        Ok(Nv12 { width, height, data })
    }
}

/// The codecs a decoder can be made for here, as bits.
pub fn decodable() -> u8 {
    static FOUND: OnceLock<u8> = OnceLock::new();
    *FOUND.get_or_init(|| {
        [Codec::H264, Codec::Hevc]
            .into_iter()
            .filter(|codec| Decoder::new(*codec, 1280, 720).is_ok())
            .fold(0, |bits, codec| bits | codec.bit())
    })
}
