use gio::prelude::*;
use glib::clone;
use gtk::prelude::*;
use std::fs::File;
use std::io::{Read, Write};
use gtk::ResponseType;
use gdk_pixbuf::Pixbuf;
use std::error::Error;
use std::sync::mpsc;
use std::cell::RefCell;
use std::rc::Rc;
use chrono::Local;
use gag::Gag;
use cpal::traits::*;
use std::time::{SystemTime, UNIX_EPOCH};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use std::thread;

use crate::gui::microphone_thread::microphone_thread;
use crate::gui::processing_thread::processing_thread;
use crate::gui::http_thread::http_thread;
use crate::gui::csv_song_history::{SongHistoryInterface, SongHistoryRecord};
use crate::gui::thread_messages::{*, GUIMessage::*};

use crate::fingerprinting::signature_format::DecodedSignature;

fn spawn_big_thread<F, T>(argument: F) -> ()
    where
        F: std::ops::FnOnce() -> T,
        F: std::marker::Send + 'static,
        T: std::marker::Send + 'static {
    thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(argument).unwrap();
}

pub fn gui_main(recording: bool) -> Result<(), Box<dyn Error>> {
    
    let application = gtk::Application::new(Some("com.github.marinm.songrec"),
        gio::ApplicationFlags::FLAGS_NONE)
        .expect("Application::new failed");
    
    application.connect_activate(move |application| {
        
        let glade_src = include_str!("interface.glade");
        let builder = gtk::Builder::from_string(glade_src);
        
        
        // We create the main window.
        
        let window: gtk::ApplicationWindow = builder.get_object("window").unwrap();
        window.set_application(Some(application));

        // We spawn required background threads, and create the
        // associated communication channels.
        
        // We use the GLib communication channel in order for
        // communication with the main GTK+ loop and the standard
        // Rust channels for other threads. An alternative would be
        // to use async-based stuff with external crates.
        
        let (gui_tx, gui_rx) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
        let (microphone_tx, microphone_rx) = mpsc::channel();
        let (processing_tx, processing_rx) = mpsc::channel();
        let (http_tx, http_rx) = mpsc::channel();
        
        let microphone_tx_2 = microphone_tx.clone();
        let microphone_tx_3 = microphone_tx.clone();
        let microphone_tx_4 = microphone_tx.clone();
        let microphone_tx_5 = microphone_tx.clone();
        let processing_tx_2 = processing_tx.clone();
        
        spawn_big_thread(clone!(@strong gui_tx => move || { // microphone_rx, processing_tx
            microphone_thread(microphone_rx, processing_tx_2, gui_tx);
        }));
        
        spawn_big_thread(clone!(@strong gui_tx => move || { // processing_rx, http_tx
            processing_thread(processing_rx, http_tx, gui_tx);
        }));
        
        spawn_big_thread(clone!(@strong gui_tx => move || { // http_rx
            http_thread(http_rx, gui_tx, microphone_tx_3);
        }));
        
        // We initialize the CSV file that will contain song history.

        let mut song_history_interface = SongHistoryInterface::new(builder.get_object("history_list_store").unwrap()).unwrap();
        
        // Obtain items from vertical box layout with a file picker button,
        // and places for song recognition information
        
        let recognize_file_button: gtk::Button = builder.get_object("recognize_file_button").unwrap();
        let spinner: gtk::Spinner = builder.get_object("spinner").unwrap();
        
        let results_frame: gtk::Frame = builder.get_object("results_frame").unwrap();
        
        let recognized_song_name: gtk::Label = builder.get_object("recognized_song_name").unwrap();
        let recognized_song_cover: gtk::Image = builder.get_object("recognized_song_cover").unwrap();
        
        let microphone_button: gtk::Button = builder.get_object("microphone_button").unwrap();
        let microphone_stop_button: gtk::Button = builder.get_object("microphone_stop_button").unwrap();
        
        let youtube_button: gtk::Button = builder.get_object("youtube_button").unwrap();
        let lure_button: gtk::Button = builder.get_object("lure_button").unwrap();
        
        let wipe_history_button: gtk::Button = builder.get_object("wipe_history_button").unwrap();
        let export_csv_button: gtk::Button = builder.get_object("export_csv_button").unwrap();
        
        // Thread-local variables to be passed across callbacks.
        
        let youtube_query: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let youtube_query_2 = youtube_query.clone();
        
        let current_signature: Rc<RefCell<Option<DecodedSignature>>> = Rc::new(RefCell::new(None));
        let current_signature_2 = current_signature.clone();
        
        // Remember about the saved last-used microphone device, if any

        let device_name_savefile = SongHistoryInterface::obtain_csv_path().unwrap()
            .replace("song_history.csv", "device_name.txt");
        
        let mut old_device_name: Option<String> = None;
        
        if let Ok(mut file) = File::open(&device_name_savefile) {
            let mut old_device_name_string = String::new();
            file.read_to_string(&mut old_device_name_string).unwrap();
            
            old_device_name = Some(old_device_name_string);
        }
        
        // List available input microphones devices in the appropriate combo box
        
        let combo_box: gtk::ComboBox = builder.get_object("microphone_source_select_box").unwrap();
        let combo_box_model: gtk::ListStore = builder.get_object("input_devices_list_store").unwrap();
        
        let current_volume_hbox: gtk::Box = builder.get_object("current_volume_hbox").unwrap();
        let current_volume_bar: gtk::ProgressBar = builder.get_object("current_volume_bar").unwrap();
        
        let host = cpal::default_host();
        
        // Avoid having alsalib polluting stderr (https://github.com/RustAudio/cpal/issues/384)
        // through disabling stderr temporarily
        
        let print_gag = Gag::stderr().unwrap();
        let mut old_device_index = 0;
        let mut current_index = 0;
        
        for device in host.input_devices().unwrap() {
            let device_name = device.name().unwrap();
            
            combo_box_model.set(&combo_box_model.append(), &[0], &[&device_name]);
            
            if old_device_name == Some(device_name) {
                old_device_index = current_index;
            }
            current_index += 1;
        }
        
        drop(print_gag);
        
        combo_box.set_active(Some(old_device_index));
        
        combo_box.connect_changed(clone!(@weak microphone_stop_button, @weak combo_box => move |_| {
            
            let device_name = combo_box.get_active_id().unwrap().to_string();
            
            // Save the selected microphone device name so that it is
            // remembered after relaunching the app
            
            let mut file = File::create(&device_name_savefile).unwrap();
            
            file.write_all(device_name.as_bytes()).unwrap();
            file.sync_all().unwrap();
            
            drop(file);
            
            if microphone_stop_button.is_visible() {
                
                // Re-launch the microphone recording with the new selected
                // device
                
                microphone_tx_4.send(MicrophoneMessage::MicrophoneRecordStop).unwrap();
                microphone_tx_4.send(MicrophoneMessage::MicrophoneRecordStart(device_name)).unwrap();
                
            }

        }));
        
        // Handle various controls
        
        recognize_file_button.connect_clicked(clone!(@weak window, @weak spinner, @weak recognize_file_button => move |_| {
            
            let file_chooser = gtk::FileChooserDialog::new(
                Some("Select a file to recognize"),
                Some(&window),
                gtk::FileChooserAction::Open
            );
            
            let file_filter = gtk::FileFilter::new();
            
            file_filter.add_pattern("*.mp3");
            file_filter.add_pattern("*.wav");
            file_filter.add_pattern("*.flac");
            file_filter.add_pattern("*.ogg");
            
            file_chooser.set_filter(&file_filter);
            
            file_chooser.add_buttons(&[
                ("Open", ResponseType::Ok),
                ("Cancel", ResponseType::Cancel)
            ]);

            let processing_tx_3 = processing_tx.clone();

            file_chooser.connect_response(move |file_chooser, response| {
                if response == ResponseType::Ok {
                    recognize_file_button.hide();
                    
                    spinner.show();
                    
                    let input_file_path = file_chooser.get_filename().expect("Couldn't get filename");
                    let input_file_string = input_file_path.to_str().unwrap().to_string();
                    
                    processing_tx_3.send(ProcessingMessage::ProcessAudioFile(input_file_string)).unwrap();
                }
                
                file_chooser.close();
            });
            file_chooser.show_all();
        
        }));
        
        microphone_button.connect_clicked(clone!(@weak microphone_button, @weak microphone_stop_button, @weak current_volume_hbox, @weak combo_box => move |_| {
            
            if let Some(device_name) = combo_box.get_active_id() {
                microphone_tx.send(MicrophoneMessage::MicrophoneRecordStart(device_name.to_owned())).unwrap();
                
                microphone_stop_button.show();
                current_volume_hbox.show();
                microphone_button.hide();
            }

        }));
        
        microphone_stop_button.connect_clicked(clone!(@weak microphone_button, @weak microphone_stop_button, @weak current_volume_hbox => move |_| {
            
            microphone_tx_2.send(MicrophoneMessage::MicrophoneRecordStop).unwrap();
            
            microphone_stop_button.hide();
            current_volume_hbox.hide();
            microphone_button.show();
            
        }));
        
        youtube_button.connect_clicked(move |_| {
            
            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            
            let youtube_query_borrow = youtube_query_2.borrow();
            
            let mut encoded_search_term: String = youtube_query_borrow.as_ref().unwrap().to_string();
            encoded_search_term = utf8_percent_encode(&encoded_search_term, NON_ALPHANUMERIC).to_string();
            encoded_search_term = encoded_search_term.replace("%20", "+");
            
            let search_url = format!("https://www.youtube.com/results?search_query={}", encoded_search_term);
            
            gtk::show_uri(None, &search_url, timestamp as u32).unwrap();
            
        });
        
        lure_button.connect_clicked(move |_| {
            
            let current_signature_borrow = current_signature_2.borrow();
    
            let mixed_source = rodio::buffer::SamplesBuffer::new::<Vec<i16>>(1, 16000, current_signature_borrow.as_ref().unwrap().to_lure().unwrap());
    
            std::thread::Builder::new().spawn(move || {
                        
                let (_stream, handle) = rodio::OutputStream::try_default().unwrap();
                let sink = rodio::Sink::try_new(&handle).unwrap();
                    
                sink.append(mixed_source);
                
                sink.sleep_until_end();
                
            }).unwrap();

        
        });
        
        wipe_history_button.connect_clicked(clone!(@strong gui_tx => move |_| {

            gui_tx.send(GUIMessage::WipeSongHistory).unwrap();

        }));
        
        export_csv_button.connect_clicked(move |_| {

            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

            gtk::show_uri(None, &format!("file://{}", SongHistoryInterface::obtain_csv_path().unwrap()), timestamp as u32).unwrap();

        });
        
        gui_rx.attach(None, clone!(@weak application, @weak window, @weak results_frame, @weak spinner, @weak recognize_file_button, @weak microphone_stop_button => @default-return Continue(true), move |gui_message| {
            
            match gui_message {
                ErrorMessage(string) => {
                    recognize_file_button.show();
                    spinner.hide();

                    if !(string == "No match for this song" && microphone_stop_button.is_visible()) {
                        let dialog = gtk::MessageDialog::new(Some(&window),
                            gtk::DialogFlags::MODAL, gtk::MessageType::Error, gtk::ButtonsType::Ok, &string);
                        dialog.connect_response(|dialog, _| dialog.close());
                        dialog.show_all();
                    }
                },
                WipeSongHistory => {
                    song_history_interface.wipe_and_save();
                },
                MicrophoneVolumePercent(percent) => {
                    current_volume_bar.set_fraction((percent / 100.0) as f64);
                },
                SongRecognized(message) => {
                    let mut youtube_query_borrow = youtube_query.borrow_mut();

                    let song_name = Some(format!("{} - {}", message.artist_name, message.song_name));

                    recognize_file_button.show();
                    spinner.hide();
        
                    if *youtube_query_borrow != song_name { // If this is already the last recognized song, don't update the display (if for example we recognized a lure we played, it would update the proposed lure to a lesser quality)
                        
                        if microphone_stop_button.is_visible() {

                            let notification = gio::Notification::new("Song recognized");
                            notification.set_body(Some(song_name.as_ref().unwrap()));

                            application.send_notification(Some("recognized-song"), &notification);
                            
                        }

                        song_history_interface.add_column_and_save(SongHistoryRecord {
                            song_name: song_name.as_ref().unwrap().to_string(),
                            album: message.album_name.as_ref().unwrap_or(&"".to_string()).to_string(),
                            recognition_date: Local::now().format("%c").to_string()
                        });

                        recognized_song_name.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(song_name.as_ref().unwrap())));
                        *youtube_query_borrow = song_name;
                        
                        let mut current_signature_borrow = current_signature.borrow_mut();
                        *current_signature_borrow = Some(*message.signature);
                        
                        results_frame.show_all();
                        
                        match message.cover_image {
                            Some(cover_data) => {
                                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(&cover_data));
                                
                                match Pixbuf::from_stream::<_, gio::Cancellable>(&stream, None) {
                                
                                    Ok(pixbuf) => {
                                        recognized_song_cover.set_from_pixbuf(Some(&pixbuf));
                                        
                                        match message.album_name {
                                            Some(value) => { recognized_song_cover.set_tooltip_text(Some(&value)) },
                                            None => { recognized_song_cover.set_tooltip_text(None) }
                                        };
                                    },
                                    Err(_) => {
                                        recognized_song_cover.hide();
                                    }
                                    
                                };
                                    
                            }
                            None => {
                                recognized_song_cover.hide();
                            }
                        };
                        
                    }
                }
                
            }
            
            Continue(true)
        }));

        // Don't forget to make all widgets visible.
        
        window.show_all();

        results_frame.hide();

        spinner.hide();

        microphone_stop_button.hide();
        current_volume_hbox.hide();

        if recording {
        
            if let Some(device_name) = combo_box.get_active_id() {
                microphone_tx_5.send(MicrophoneMessage::MicrophoneRecordStart(device_name.to_owned())).unwrap();
                
                microphone_stop_button.show();
                current_volume_hbox.show();
                microphone_button.hide();
            }
        }
        
    });
    
    application.run(&[]);
    
    Ok(())
}
