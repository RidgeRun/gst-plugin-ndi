#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use glib;
use gst;
use gst::prelude::*;
use gst_audio;
use gst_base::prelude::*;

use gobject_subclass::object::*;
use gst_plugin::base_src::*;
use gst_plugin::element::*;

use std::sync::Mutex;
use std::{i32, u32, u64};

use std::ptr;

use connect_ndi;
use ndi_struct;
use ndisys::*;
use stop_ndi;

use hashmap_receivers;

#[derive(Debug, Clone)]
struct Settings {
    stream_name: String,
    ip: String,
    id_receiver: i8,
    latency: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stream_name: String::from("Fixed ndi stream name"),
            ip: String::from(""),
            id_receiver: 0,
            latency: 0,
        }
    }
}

static PROPERTIES: [Property; 3] = [
    Property::String(
        "stream-name",
        "Sream Name",
        "Name of the streaming device",
        None,
        PropertyMutability::ReadWrite,
    ),
    Property::String(
        "ip",
        "Stream IP",
        "Stream IP",
        None,
        PropertyMutability::ReadWrite,
    ),
    Property::UInt64(
        "latency",
        "Latency",
        "Latency",
        (0, u64::MAX),
        0,
        PropertyMutability::ReadWrite,
    ),
];

struct State {
    info: Option<gst_audio::AudioInfo>,
}

impl Default for State {
    fn default() -> State {
        State { info: None }
    }
}

struct TimestampData {
    offset: u64,
}

struct NdiAudioSrc {
    cat: gst::DebugCategory,
    settings: Mutex<Settings>,
    state: Mutex<State>,
    timestamp_data: Mutex<TimestampData>,
}

impl NdiAudioSrc {
    fn new(element: &BaseSrc) -> Box<BaseSrcImpl<BaseSrc>> {
        element.set_live(true);
        element.set_format(gst::Format::Time);

        Box::new(Self {
            cat: gst::DebugCategory::new(
                "ndiaudiosrc",
                gst::DebugColorFlags::empty(),
                "NewTek NDI Audio Source",
            ),
            settings: Mutex::new(Default::default()),
            state: Mutex::new(Default::default()),
            timestamp_data: Mutex::new(TimestampData { offset: 0 }),
        })
    }

    fn class_init(klass: &mut BaseSrcClass) {
        klass.set_metadata(
            "NewTek NDI Audio Source",
            "Source",
            "NewTek NDI audio source",
            "Ruben Gonzalez <rubenrua@teltek.es>, Daniel Vilar <daniel.peiteado@teltek.es>",
        );

        let caps = gst::Caps::new_simple(
            "audio/x-raw",
            &[
                (
                    "format",
                    &gst::List::new(&[
                        //TODO add more formats?
                        //&gst_audio::AUDIO_FORMAT_F32.to_string(),
                        //&gst_audio::AUDIO_FORMAT_F64.to_string(),
                        &gst_audio::AUDIO_FORMAT_S16.to_string(),
                    ]),
                ),
                ("rate", &gst::IntRange::<i32>::new(1, i32::MAX)),
                ("channels", &gst::IntRange::<i32>::new(1, i32::MAX)),
                ("layout", &"interleaved"),
            ],
        );

        let src_pad_template = gst::PadTemplate::new(
            "src",
            gst::PadDirection::Src,
            gst::PadPresence::Always,
            &caps,
        );
        klass.add_pad_template(src_pad_template);

        klass.install_properties(&PROPERTIES);
    }
}

impl ObjectImpl<BaseSrc> for NdiAudioSrc {
    fn set_property(&self, obj: &glib::Object, id: u32, value: &glib::Value) {
        let prop = &PROPERTIES[id as usize];
        let element = obj.clone().downcast::<BaseSrc>().unwrap();

        match *prop {
            Property::String("stream-name", ..) => {
                let mut settings = self.settings.lock().unwrap();
                let stream_name = value.get().unwrap();
                gst_debug!(
                    self.cat,
                    obj: &element,
                    "Changing stream-name from {} to {}",
                    settings.stream_name,
                    stream_name
                );
                settings.stream_name = stream_name;
                drop(settings);

                let _ =
                    element.post_message(&gst::Message::new_latency().src(Some(&element)).build());
            }
            Property::String("ip", ..) => {
                let mut settings = self.settings.lock().unwrap();
                let ip = value.get().unwrap();
                gst_debug!(
                    self.cat,
                    obj: &element,
                    "Changing ip from {} to {}",
                    settings.ip,
                    ip
                );
                settings.ip = ip;
                drop(settings);

                let _ =
                    element.post_message(&gst::Message::new_latency().src(Some(&element)).build());
            }
            Property::UInt64("latency", ..) => {
                let mut settings = self.settings.lock().unwrap();
                let latency = value.get().unwrap();
                gst_debug!(
                    self.cat,
                    obj: &element,
                    "Changing latency from {} to {}",
                    settings.latency,
                    latency
                );
                settings.latency = latency;
                drop(settings);
            }
            _ => unimplemented!(),
        }
    }

    fn get_property(&self, _obj: &glib::Object, id: u32) -> Result<glib::Value, ()> {
        let prop = &PROPERTIES[id as usize];

        match *prop {
            Property::String("stream-name", ..) => {
                let settings = self.settings.lock().unwrap();
                Ok(settings.stream_name.to_value())
            }
            Property::String("ip", ..) => {
                let settings = self.settings.lock().unwrap();
                Ok(settings.ip.to_value())
            }
            Property::UInt64("latency", ..) => {
                let settings = self.settings.lock().unwrap();
                Ok(settings.latency.to_value())
            }
            _ => unimplemented!(),
        }
    }
}

impl ElementImpl<BaseSrc> for NdiAudioSrc {
    fn change_state(
        &self,
        element: &BaseSrc,
        transition: gst::StateChange,
    ) -> gst::StateChangeReturn {
        if transition == gst::StateChange::PausedToPlaying {
            let receivers = hashmap_receivers.lock().unwrap();
            let settings = self.settings.lock().unwrap();

            let receiver = receivers.get(&settings.id_receiver).unwrap();
            let recv = &receiver.ndi_instance;
            let pNDI_recv = recv.recv;

            let audio_frame: NDIlib_audio_frame_v2_t = Default::default();

            let mut frame_type: NDIlib_frame_type_e = NDIlib_frame_type_e::NDIlib_frame_type_none;
            unsafe {
                while frame_type != NDIlib_frame_type_e::NDIlib_frame_type_audio {
                    frame_type = NDIlib_recv_capture_v2(
                        pNDI_recv,
                        ptr::null(),
                        &audio_frame,
                        ptr::null(),
                        1000,
                    );
                }

                if ndi_struct.initial_timestamp <= audio_frame.timestamp as u64
                    || ndi_struct.initial_timestamp == 0
                {
                    ndi_struct.initial_timestamp = audio_frame.timestamp as u64;
                }
            }
        }
        element.parent_change_state(transition)
    }
}

impl BaseSrcImpl<BaseSrc> for NdiAudioSrc {
    fn set_caps(&self, element: &BaseSrc, caps: &gst::CapsRef) -> bool {
        let info = match gst_audio::AudioInfo::from_caps(caps) {
            None => return false,
            Some(info) => info,
        };

        gst_debug!(self.cat, obj: element, "Configuring for caps {}", caps);

        let mut state = self.state.lock().unwrap();
        state.info = Some(info);

        true
    }

    fn start(&self, element: &BaseSrc) -> bool {
        *self.state.lock().unwrap() = Default::default();

        let mut settings = self.settings.lock().unwrap();
        settings.id_receiver = connect_ndi(
            self.cat,
            element,
            &settings.ip.clone(),
            &settings.stream_name.clone(),
        );

        settings.id_receiver != 0
    }

    fn stop(&self, element: &BaseSrc) -> bool {
        *self.state.lock().unwrap() = Default::default();

        let settings = self.settings.lock().unwrap();
        stop_ndi(self.cat, element, settings.id_receiver);
        // Commented because when adding ndi destroy stopped in this line
        //*self.state.lock().unwrap() = Default::default();
        true
    }

    fn query(&self, element: &BaseSrc, query: &mut gst::QueryRef) -> bool {
        use gst::QueryView;
        if let QueryView::Scheduling(ref mut q) = query.view_mut() {
            q.set(gst::SchedulingFlags::SEQUENTIAL, 1, -1, 0);
            q.add_scheduling_modes(&[gst::PadMode::Push]);
            return true;
        }
        if let QueryView::Latency(ref mut q) = query.view_mut() {
            let settings = &*self.settings.lock().unwrap();
            let state = self.state.lock().unwrap();

            if let Some(ref _info) = state.info {
                let latency = settings.latency;
                gst_debug!(self.cat, obj: element, "setting latency {} ms", latency);
                q.set(
                    true,
                    gst::MSECOND.mul_div_floor(latency, 1).unwrap(),
                    gst::CLOCK_TIME_NONE,
                );
                return true;
            } else {
                return false;
            }
        }
        BaseSrcBase::parent_query(element, query)
    }

    fn fixate(&self, element: &BaseSrc, caps: gst::Caps) -> gst::Caps {
        let receivers = hashmap_receivers.lock().unwrap();
        let settings = self.settings.lock().unwrap();

        let receiver = receivers.get(&settings.id_receiver).unwrap();

        let recv = &receiver.ndi_instance;
        let pNDI_recv = recv.recv;

        let audio_frame: NDIlib_audio_frame_v2_t = Default::default();

        let mut frame_type: NDIlib_frame_type_e = NDIlib_frame_type_e::NDIlib_frame_type_none;
        while frame_type != NDIlib_frame_type_e::NDIlib_frame_type_audio {
            unsafe {
                frame_type =
                    NDIlib_recv_capture_v2(pNDI_recv, ptr::null(), &audio_frame, ptr::null(), 1000);
            }
        }

        let mut caps = gst::Caps::truncate(caps);
        {
            let caps = caps.make_mut();
            let s = caps.get_mut_structure(0).unwrap();
            s.fixate_field_nearest_int("rate", audio_frame.sample_rate);
            s.fixate_field_nearest_int("channels", audio_frame.no_channels);
            s.fixate_field_str("layout", "interleaved");
        }

        let _ = element.post_message(&gst::Message::new_latency().src(Some(element)).build());
        element.parent_fixate(caps)
    }

    fn create(
        &self,
        element: &BaseSrc,
        _offset: u64,
        _length: u32,
    ) -> Result<gst::Buffer, gst::FlowReturn> {
        let _settings = &*self.settings.lock().unwrap();

        let mut timestamp_data = self.timestamp_data.lock().unwrap();

        let state = self.state.lock().unwrap();
        let _info = match state.info {
            None => {
                gst_element_error!(element, gst::CoreError::Negotiation, ["Have no caps yet"]);
                return Err(gst::FlowReturn::NotNegotiated);
            }
            Some(ref info) => info.clone(),
        };
        let receivers = hashmap_receivers.lock().unwrap();

        let recv = &receivers.get(&_settings.id_receiver).unwrap().ndi_instance;
        let pNDI_recv = recv.recv;

        let pts: u64;
        let audio_frame: NDIlib_audio_frame_v2_t = Default::default();

        unsafe {
            let time = ndi_struct.initial_timestamp;

            let mut skip_frame = true;
            while skip_frame {
                let frame_type =
                    NDIlib_recv_capture_v2(pNDI_recv, ptr::null(), &audio_frame, ptr::null(), 1000);
                if frame_type == NDIlib_frame_type_e::NDIlib_frame_type_none
                    || frame_type == NDIlib_frame_type_e::NDIlib_frame_type_error
                {
                    gst_element_error!(element, gst::ResourceError::Read, ["NDI frame type none received, assuming that the source closed the stream...."]);
                    return Err(gst::FlowReturn::CustomError);
                }
                if time >= (audio_frame.timestamp as u64) {
                    gst_debug!(self.cat, obj: element, "Frame timestamp ({:?}) is lower than received in the first frame from NDI ({:?}), so skiping...", (audio_frame.timestamp as u64), time);
                } else {
                    skip_frame = false;
                }
            }

            pts = audio_frame.timestamp as u64 - time;

            let buff_size = (audio_frame.channel_stride_in_bytes) as usize;
            let mut buffer = gst::Buffer::with_size(buff_size).unwrap();
            {
                // Newtek NDI yields times in 100ns intervals since the Unix Time
                let pts: gst::ClockTime = (pts * 100).into();

                let duration: gst::ClockTime = (((f64::from(audio_frame.no_samples)
                    / f64::from(audio_frame.sample_rate))
                    * 1_000_000_000.0) as u64)
                    .into();

                let buffer = buffer.get_mut().unwrap();
                buffer.set_pts(pts);
                buffer.set_duration(duration);
                buffer.set_offset(timestamp_data.offset);
                timestamp_data.offset += audio_frame.no_samples as u64;
                buffer.set_offset_end(timestamp_data.offset);

                let mut dst: NDIlib_audio_frame_interleaved_16s_t = Default::default();
                dst.reference_level = 0;
                dst.p_data = buffer.map_writable().unwrap().as_mut_slice().as_mut_ptr() as *mut i16;
                NDIlib_util_audio_to_interleaved_16s_v2(&audio_frame, &mut dst);
            }

            gst_debug!(self.cat, obj: element, "Produced buffer {:?}", buffer);

            Ok(buffer)
        }
    }
}

struct NdiAudioSrcStatic;

impl ImplTypeStatic<BaseSrc> for NdiAudioSrcStatic {
    fn get_name(&self) -> &str {
        "NdiAudioSrc"
    }

    fn new(&self, element: &BaseSrc) -> Box<BaseSrcImpl<BaseSrc>> {
        NdiAudioSrc::new(element)
    }

    fn class_init(&self, klass: &mut BaseSrcClass) {
        NdiAudioSrc::class_init(klass);
    }
}

pub fn register(plugin: &gst::Plugin) {
    let type_ = register_type(NdiAudioSrcStatic);
    gst::Element::register(plugin, "ndiaudiosrc", 0, type_);
}
