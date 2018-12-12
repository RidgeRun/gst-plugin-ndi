#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use glib;
use glib::subclass;
use glib::subclass::prelude::*;
use gst;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_audio;
use gst_base;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;

use std::sync::Mutex;
use std::{i32, u32};

use std::ptr;

use connect_ndi;
use ndi_struct;
use ndisys::*;
use stop_ndi;

use hashmap_receivers;
use byte_slice_cast::AsMutSliceOf;

#[derive(Debug, Clone)]
struct Settings {
    stream_name: String,
    ip: String,
    loss_threshold: u32,
    id_receiver: i8,
    latency: Option<gst::ClockTime>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stream_name: String::from("Fixed ndi stream name"),
            ip: String::from(""),
            loss_threshold: 40000,
            id_receiver: 0,
            latency: None,
        }
    }
}

static PROPERTIES: [subclass::Property; 3] = [
subclass::Property("stream-name", || {
    glib::ParamSpec::string(
        "stream-name",
        "Sream Name",
        "Name of the streaming device",
        None,
        glib::ParamFlags::READWRITE,
    )
}),
subclass::Property("ip", || {
    glib::ParamSpec::string(
        "ip",
        "Stream IP",
        "IP of the streaming device. Ex: 127.0.0.1:5961",
        None,
        glib::ParamFlags::READWRITE,
    )
}),
subclass::Property("loss-threshold", || {
    glib::ParamSpec::uint(
        "loss-threshold",
        "Loss threshold",
        "Loss threshold",
        0,
        200000,
        40000,
        glib::ParamFlags::READWRITE,
    )
}),
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
    count_frame_none: u32,
}

struct NdiAudioSrc {
    cat: gst::DebugCategory,
    settings: Mutex<Settings>,
    state: Mutex<State>,
    timestamp_data: Mutex<TimestampData>,
}

impl ObjectSubclass for NdiAudioSrc {

    const NAME: &'static str = "NdiAudioSrc";
    type ParentType = gst_base::BaseSrc;
    type Instance = gst::subclass::ElementInstanceStruct<Self>;
    type Class = subclass::simple::ClassStruct<Self>;

    glib_object_subclass!();

    fn new() -> Self {
        Self {
            cat: gst::DebugCategory::new(
                "ndiaudiosrc",
                gst::DebugColorFlags::empty(),
                "NewTek NDI Audio Source",
            ),
            settings: Mutex::new(Default::default()),
            state: Mutex::new(Default::default()),
            timestamp_data: Mutex::new(TimestampData { offset: 0, count_frame_none: 0 }),
        }
    }

    fn class_init(klass: &mut subclass::simple::ClassStruct<Self>) {
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
                ("channel-mask", &gst::Bitmask::new(0)),
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

    impl ObjectImpl for NdiAudioSrc {
        glib_object_impl!();

        fn constructed(&self, obj: &glib::Object) {
            self.parent_constructed(obj);

            let basesrc = obj.downcast_ref::<gst_base::BaseSrc>().unwrap();
            // Initialize live-ness and notify the base class that
            // we'd like to operate in Time format
            basesrc.set_live(true);
            basesrc.set_format(gst::Format::Time);
        }

        fn set_property(&self, obj: &glib::Object, id: usize, value: &glib::Value) {
            let prop = &PROPERTIES[id];
            let basesrc = obj.downcast_ref::<gst_base::BaseSrc>().unwrap();

            match *prop {
                subclass::Property("stream-name", ..) => {
                    let mut settings = self.settings.lock().unwrap();
                    let stream_name = value.get().unwrap();
                    gst_debug!(
                        self.cat,
                        obj: basesrc,
                        "Changing stream-name from {} to {}",
                        settings.stream_name,
                        stream_name
                    );
                    settings.stream_name = stream_name;
                    drop(settings);
                }
                subclass::Property("ip", ..) => {
                    let mut settings = self.settings.lock().unwrap();
                    let ip = value.get().unwrap();
                    gst_debug!(
                        self.cat,
                        obj: basesrc,
                        "Changing ip from {} to {}",
                        settings.ip,
                        ip
                    );
                    settings.ip = ip;
                    drop(settings);
                }
                subclass::Property("loss-threshold", ..) => {
                    let mut settings = self.settings.lock().unwrap();
                    let loss_threshold = value.get().unwrap();
                    gst_debug!(
                        self.cat,
                        obj: basesrc,
                        "Changing loss threshold from {} to {}",
                        settings.loss_threshold,
                        loss_threshold
                    );
                    settings.loss_threshold = loss_threshold;
                    drop(settings);
                }
                _ => unimplemented!(),
            }
        }

        fn get_property(&self, _obj: &glib::Object, id: usize) -> Result<glib::Value, ()> {
            let prop = &PROPERTIES[id];

            match *prop {
                subclass::Property("stream-name", ..) => {
                    let settings = self.settings.lock().unwrap();
                    Ok(settings.stream_name.to_value())
                }
                subclass::Property("ip", ..) => {
                    let settings = self.settings.lock().unwrap();
                    Ok(settings.ip.to_value())
                }
                subclass::Property("loss-threshold", ..) => {
                    let settings = self.settings.lock().unwrap();
                    Ok(settings.loss_threshold.to_value())
                }
                _ => unimplemented!(),
            }
        }
    }

    impl ElementImpl for NdiAudioSrc {
        fn change_state(
            &self,
            element: &gst::Element,
            transition: gst::StateChange,
        ) -> gst::StateChangeReturn {
            if transition == gst::StateChange::PausedToPlaying {
                let mut receivers = hashmap_receivers.lock().unwrap();
                let settings = self.settings.lock().unwrap();

                let receiver = receivers.get_mut(&settings.id_receiver).unwrap();
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
                        gst_debug!(self.cat, obj: element, "NDI audio frame received: {:?}", audio_frame);
                    }

                    if receiver.initial_timestamp <= audio_frame.timestamp as u64
                    || receiver.initial_timestamp == 0
                    {
                        receiver.initial_timestamp = audio_frame.timestamp as u64;
                    }
                    gst_debug!(self.cat, obj: element, "Setting initial timestamp to {}", receiver.initial_timestamp);
                }
            }
            self.parent_change_state(element, transition)
        }
    }

    impl BaseSrcImpl for NdiAudioSrc {
        fn set_caps(&self, element: &gst_base::BaseSrc, caps: &gst::CapsRef) -> bool {
            let info = match gst_audio::AudioInfo::from_caps(caps) {
                None => return false,
                Some(info) => info,
            };

            gst_debug!(self.cat, obj: element, "Configuring for caps {}", caps);

            let mut state = self.state.lock().unwrap();
            state.info = Some(info);

            true
        }

        fn start(&self, element: &gst_base::BaseSrc) -> bool {
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

        fn stop(&self, element: &gst_base::BaseSrc) -> bool {
            *self.state.lock().unwrap() = Default::default();

            let settings = self.settings.lock().unwrap();
            stop_ndi(self.cat, element, settings.id_receiver);
            // Commented because when adding ndi destroy stopped in this line
            //*self.state.lock().unwrap() = Default::default();
            true
        }

        fn query(&self, element: &gst_base::BaseSrc, query: &mut gst::QueryRef) -> bool {
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
                    let latency = settings.latency.unwrap();
                    gst_debug!(self.cat, obj: element, "Returning latency {}", latency);
                    q.set(true, latency, gst::CLOCK_TIME_NONE);
                    return true;
                } else {
                    return false;
                }
            }
            BaseSrcImpl::parent_query(self, element, query)
        }

        fn fixate(&self, element: &gst_base::BaseSrc, caps: gst::Caps) -> gst::Caps {
            let receivers = hashmap_receivers.lock().unwrap();
            let mut settings = self.settings.lock().unwrap();

            let receiver = receivers.get(&settings.id_receiver).unwrap();

            let recv = &receiver.ndi_instance;
            let pNDI_recv = recv.recv;

            let audio_frame: NDIlib_audio_frame_v2_t = Default::default();

            let mut frame_type: NDIlib_frame_type_e = NDIlib_frame_type_e::NDIlib_frame_type_none;
            while frame_type != NDIlib_frame_type_e::NDIlib_frame_type_audio {
                unsafe {
                    frame_type =
                    NDIlib_recv_capture_v2(pNDI_recv, ptr::null(), &audio_frame, ptr::null(), 1000);
                    gst_debug!(self.cat, obj: element, "NDI audio frame received: {:?}", audio_frame);
                }
            }

            let no_samples = audio_frame.no_samples as u64;
            let audio_rate = audio_frame.sample_rate;
            settings.latency = gst::SECOND.mul_div_floor(no_samples, audio_rate as u64);

            let mut caps = gst::Caps::truncate(caps);
            {
                let caps = caps.make_mut();
                let s = caps.get_mut_structure(0).unwrap();
                s.fixate_field_nearest_int("rate", audio_rate);
                s.fixate_field_nearest_int("channels", audio_frame.no_channels);
                s.fixate_field_str("layout", "interleaved");
                s.set_value("channel-mask", gst::Bitmask::new(gst_audio::AudioChannelPosition::get_fallback_mask(audio_frame.no_channels as u32)).to_send_value());
            }

            let _ = element.post_message(&gst::Message::new_latency().src(Some(element)).build());
            self.parent_fixate(element, caps)
        }

        fn create(
            &self,
            element: &gst_base::BaseSrc,
            _offset: u64,
            _length: u32,
        ) -> Result<gst::Buffer, gst::FlowError> {
            let _settings = &*self.settings.lock().unwrap();

            let mut timestamp_data = self.timestamp_data.lock().unwrap();

            let state = self.state.lock().unwrap();
            let _info = match state.info {
                None => {
                    gst_element_error!(element, gst::CoreError::Negotiation, ["Have no caps yet"]);
                    return Err(gst::FlowError::NotNegotiated);
                }
                Some(ref info) => info.clone(),
            };
            let receivers = hashmap_receivers.lock().unwrap();

            let recv = &receivers.get(&_settings.id_receiver).unwrap().ndi_instance;
            let pNDI_recv = recv.recv;

            let pts: u64;
            let audio_frame: NDIlib_audio_frame_v2_t = Default::default();

            unsafe {
                let time = receivers.get(&_settings.id_receiver).unwrap().initial_timestamp;

                let mut skip_frame = true;
                while skip_frame {
                    let frame_type =
                    NDIlib_recv_capture_v2(pNDI_recv, ptr::null(), &audio_frame, ptr::null(), 0);
                    if (frame_type == NDIlib_frame_type_e::NDIlib_frame_type_none && _settings.loss_threshold != 0)
                    || frame_type == NDIlib_frame_type_e::NDIlib_frame_type_error
                    {
                        if timestamp_data.count_frame_none < _settings.loss_threshold{
                            timestamp_data.count_frame_none += 1;
                            gst_debug!(self.cat, obj: element, "No audio frame received, sending empty buffer, count of none frames since last audio frame: {}", timestamp_data.count_frame_none);
                            let buffer = gst::Buffer::with_size(0).unwrap();
                            return Ok(buffer)
                        }
                        gst_element_error!(element, gst::ResourceError::Read, ["NDI frame type none or error received, assuming that the source closed the stream...."]);
                        return Err(gst::FlowError::CustomError);
                    }
                    else if frame_type == NDIlib_frame_type_e::NDIlib_frame_type_none && _settings.loss_threshold == 0{
                            gst_debug!(self.cat, obj: element, "No audio frame received, sending empty buffer");
                            let buffer = gst::Buffer::with_size(0).unwrap();
                            return Ok(buffer)
                        }

                    if time >= (audio_frame.timestamp as u64) {
                        gst_debug!(self.cat, obj: element, "Frame timestamp ({:?}) is lower than received in the first frame from NDI ({:?}), so skiping...", (audio_frame.timestamp as u64), time);
                    } else {
                        skip_frame = false;
                    }
                }

                gst_log!(self.cat, obj: element, "NDI audio frame received: {:?}", (audio_frame));

                pts = audio_frame.timestamp as u64 - time;

                gst_log!(self.cat, obj: element, "Calculated pts for audio frame: {:?}", (pts));

                // We multiply by 2 because is the size in bytes of an i16 variable
                let buff_size = (audio_frame.no_samples * 2 * audio_frame.no_channels) as usize;
                let mut buffer = gst::Buffer::with_size(buff_size).unwrap();
                {
                    if ndi_struct.start_pts == gst::ClockTime(Some(0)) {
                        ndi_struct.start_pts =
                        element.get_clock().unwrap().get_time() - element.get_base_time();
                    }

                    let buffer = buffer.get_mut().unwrap();

                    // Newtek NDI yields times in 100ns intervals since the Unix Time
                    let pts: gst::ClockTime = (pts * 100).into();
                    buffer.set_pts(pts + ndi_struct.start_pts);

                    let duration: gst::ClockTime = (((f64::from(audio_frame.no_samples)
                    / f64::from(audio_frame.sample_rate))
                    * 1_000_000_000.0) as u64)
                    .into();
                    buffer.set_duration(duration);

                    buffer.set_offset(timestamp_data.offset);
                    timestamp_data.offset += audio_frame.no_samples as u64;
                    buffer.set_offset_end(timestamp_data.offset);

                    let mut dst: NDIlib_audio_frame_interleaved_16s_t = Default::default();
                    dst.reference_level = 0;
                    dst.p_data = buffer.map_writable().unwrap().as_mut_slice_of::<i16>().unwrap().as_mut_ptr();
                    NDIlib_util_audio_to_interleaved_16s_v2(&audio_frame, &mut dst);
                }

                timestamp_data.count_frame_none = 0;
                gst_log!(self.cat, obj: element, "Produced buffer {:?}", buffer);

                Ok(buffer)
            }
        }
    }

    pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
        gst::Element::register(plugin, "ndiaudiosrc", 0, NdiAudioSrc::get_type())
    }
