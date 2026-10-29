//! Localizable explanations for UI chrome. Authored names are never translated.
use eframe::egui;

const HINTS: &[(&str, &str)] = &[
    (
        "Hide",
        "Hide this dock without applying or discarding its draft.",
    ),
    (
        "Scene Notes Dock",
        "Show or hide the plain-text Scene Notes dock. Hiding retains its draft and authored notes.",
    ),
    (
        "Notes for this scene",
        "Edit plain-text cues for this scene. Apply notes commits them; Cancel edits discards the draft.",
    ),
    ("Layout", "Choose a dock arrangement for this window."),
    (
        "Dock sizes",
        "Adjust dock dimensions using the keyboard or pointer.",
    ),
    (
        "Basic.MainMenu.Docks.LockDocks",
        "Prevent dock resizing while keeping source, mixer and output controls usable.",
    ),
    (
        "Basic.MainMenu.Docks.SideDocks",
        "Let the left docks extend beside the bottom docks.",
    ),
    (
        "Reset Layout",
        "Restore default dock widths, visibility and compact selection.",
    ),
    (
        "Expander preset: Expander",
        "Load the Expander preset: 2:1 ratio, 10 ms attack and 50 ms release.",
    ),
    (
        "Expander preset: Gate",
        "Load the Gate preset: 10:1 ratio, 10 ms attack and 125 ms release.",
    ),
    (
        "RMS detector",
        "Detect average signal power independently for each channel.",
    ),
    (
        "Peak detector",
        "Detect instantaneous peaks independently for each channel.",
    ),
    (
        "Add OBS Limiter",
        "Add the OBS linked-peak limiter to this draft. Apply publishes the chain; no filter is inserted automatically.",
    ),
    (
        "Add OBS Compressor",
        "Add the OBS hard-knee peak compressor to this draft. It is separate from the Craft Compressor.",
    ),
    (
        "Add OBS Expander",
        "Add the OBS per-channel RMS/peak expander to this draft. Its Gate preset does not replace the Craft Gate.",
    ),
    (
        "document-picker-path",
        "Enter the exact file to open, recover or import.",
    ),
    ("color-source-name", "Name this shared Color Source."),
    (
        "stream-server-url",
        "Enter rtmp://host[:port]/application. IPv6 uses brackets. No DNS or connection occurs while editing.",
    ),
    (
        "stream-key",
        "Enter the exact stream name or key. Spaces and query text are preserved; the server URL is separate.",
    ),
    (
        "stream-video-bitrate",
        "Request an average H.264 bitrate from 64 to 100000 kbit/s. Constant bitrate is not guaranteed.",
    ),
    (
        "stream-keyframe-interval",
        "Choose 1 to 10 seconds. Native GOP frames use the OBS integer frame-rate calculation.",
    ),
    (
        "Include Program Audio",
        "Publish final Program audio as AAC. Preview-only sources are excluded; the format is checked at Start.",
    ),
    (
        "Show Stream Key",
        "Show or hide the key in this editor without changing its stored value.",
    ),
    (
        "Clear Server Configuration",
        "Clear the server and key in this draft while keeping encoder choices. Apply is still required.",
    ),
    (
        "Restore Stream Defaults",
        "Restore an unconfigured server and default encoder choices in this draft. Apply is still required.",
    ),
    (
        "Save As",
        "Choose and explicitly confirm a different native save destination.",
    ),
    (
        "Open / Recover target",
        "Choose a document to open or recover. Save uses the active native document binding.",
    ),
    (
        "native-save-path",
        "Choose a native SkyLive filename. This path is separate from the Open target.",
    ),
    (
        "Save and continue",
        "Save the current native document before replacing it.",
    ),
    (
        "Discard and continue",
        "Replace the current document without saving its changes.",
    ),
    (
        "Save to this path",
        "Confirm this exact native destination, including replacement of an existing file.",
    ),
    (
        "Color Key",
        "Remove a background using OBS Color Key v2 RGB distance and SDR color controls.",
    ),
    (
        "Chroma Key",
        "Remove a background using OBS Chroma Key v2 spatial sampling and spill reduction.",
    ),
    (
        "Key Color Type",
        "Choose a preset background color or use the retained custom color.",
    ),
    ("Similarity", "Set the OBS key similarity from 1 to 1000."),
    (
        "Smoothness",
        "Soften the OBS key transition from 1 to 1000.",
    ),
    (
        "Spill Reduction",
        "Reduce background chroma spill using the OBS mask.",
    ),
    (
        "video-filter-name",
        "Name this filter. Names must be unique within this source.",
    ),
    (
        "Add Gain",
        "Add the built-in OBS Gain filter. It applies the same dB gain to every channel without early clipping.",
    ),
    (
        "Add Invert Polarity",
        "Add the built-in OBS Invert Polarity filter. It reverses every channel's sign without changing timing.",
    ),
    (
        "Interface scale",
        "Scale interface text and controls from 75% to 200%. Video resolution stays unchanged.",
    ),
    (
        "Restore default appearance",
        "Choose Yami and 100% scale in this draft.",
    ),
    (
        "Color Correction",
        "Adjust SDR gamma, contrast, brightness, saturation, hue, opacity and color washes.",
    ),
    ("Gamma", "Adjust the gamma exponent in linear light."),
    (
        "Saturation",
        "Adjust color saturation using OBS luminance weights.",
    ),
    (
        "Hue Shift",
        "Rotate hue around the RGB diagonal in degrees.",
    ),
    (
        "Color Multiply",
        "Multiply RGB by this color in linear light.",
    ),
    ("Color Add", "Add this color in linear light."),
    (
        "Global Audio Devices",
        "Manage the six global audio slots. Confirm or cancel the format draft before opening device settings.",
    ),
    (
        "Add New",
        "Create and configure a new audio source for this global slot.",
    ),
    ("Clear", "Remove this shortcut assignment."),
    (
        "Restore default shortcuts",
        "Restore Ctrl/Command+Z, Ctrl/Command+Shift+Z and Ctrl/Command+S in this draft.",
    ),
    (
        "actionAdvAudioProperties",
        "Review audio source activity, volume, mute and filter settings.",
    ),
    (
        "actionMixerToolbarAdvAudio",
        "Review audio source activity, volume, mute and filter settings.",
    ),
    ("Active", "Include this audio source in the selected scene."),
    (
        "Collapse Group",
        "Hide this group's child rows in Sources. Its sources keep their current visibility.",
    ),
    ("Expand Group", "Show this group's child rows in Sources."),
    (
        "Add Gate",
        "Add a noise gate to attenuate quiet sound below a threshold.",
    ),
    (
        "Add Simple EQ",
        "Add a three-band equalizer with low, mid and high controls.",
    ),
    (
        "Media Source",
        "Play a local MP4 or MOV file as a video or audio source.",
    ),
    (
        "Group",
        "Add an empty group, then move source items into it.",
    ),
    (
        "Group Selected Items",
        "Group selected source rows. Ctrl-click or Command-click rows to select more than one.",
    ),
    (
        "Move into Group",
        "Move this item into an existing sibling group.",
    ),
    (
        "Move out of Group",
        "Move this item to the top of its parent group's scene.",
    ),
    (
        "Ungroup",
        "Move the group's children into its parent and remove the group.",
    ),
    (
        "media-source-name",
        "Name this media source so you can find it in Sources.",
    ),
    (
        "media-source-path",
        "Choose a local MP4 or MOV file. The file is checked when you apply.",
    ),
    ("Loop", "Play the media again when it reaches the end."),
    (
        "Restart playback when source becomes active",
        "Start from the beginning when this source becomes active in Program.",
    ),
    (
        "Close file when inactive",
        "Release the media file when the source is no longer showing.",
    ),
    (
        "Show nothing when playback ends",
        "Clear the last video frame after playback reaches the end.",
    ),
    (
        "recording-directory",
        "Set the folder for new recordings. Its availability is checked when recording starts.",
    ),
    (
        "recording-video-bitrate",
        "Request an average H.264 bitrate from 100 to 200000 kbit/s. Constant bitrate is not guaranteed.",
    ),
    (
        "Record Audio",
        "Include the Program audio mix, encoded as mono or stereo AAC.",
    ),
    (
        "Prefer hardware encoding",
        "Prefer a compatible hardware encoder when available. This preference does not guarantee hardware encoding.",
    ),
    (
        "group-source-name",
        "Name this group so you can find it in Sources.",
    ),
    (
        "Source Actions",
        "Open source rename, filters, transform and ordering actions.",
    ),
    (
        "Scene Actions",
        "Open scene rename, duplicate, transition and ordering actions.",
    ),
    (
        "Automatic",
        "Choose the display capture method automatically. Compatibility fallbacks are reported.",
    ),
    (
        "DXGI Desktop Duplication",
        "Capture a Windows display using DXGI Desktop Duplication. Cursor capture is not supported by this method.",
    ),
    (
        "GDI compatibility",
        "Capture a Windows display using GDI compatibility, including the cursor when enabled.",
    ),
    (
        "Windows Graphics Capture (unavailable)",
        "Windows Graphics Capture has not been implemented.",
    ),
    (
        "Video Capture Device",
        "Add a camera video source to the selected scene.",
    ),
    ("camera-source-name", "Name of this video capture source"),
    (
        "Refresh Cameras",
        "Refresh device names without starting video capture",
    ),
    (
        "Query Modes",
        "Querying modes may briefly activate the selected camera driver",
    ),
    (
        "Deactivate when not showing",
        "Release the camera while no active scene or Studio Preview shows this source",
    ),
    (
        "Stop Camera Capture",
        "Stop all camera streams; device shutdown is acknowledged separately",
    ),
    (
        "Add Filter",
        "Add a video effect to this source. A stack supports at most 32 filters.",
    ),
    ("Filters", "Edit the selected source's video filters."),
    (
        "Keep Keyed Colors",
        "Keep matching colors instead of removing them.",
    ),
    (
        "Reset Filter",
        "Restore this filter's parameter defaults without changing its enabled state.",
    ),
    (
        "Use Legacy",
        "Use the legacy brightness and contrast calculation.",
    ),
    (
        "Add Compressor",
        "Add the bundled compressor to this source's filter chain.",
    ),
    (
        "Peak",
        "Detect the loudest channel's sample peak; this is not true-peak limiting.",
    ),
    (
        "RMS",
        "Detect the loudest channel's smoothed mean-square level.",
    ),
    (
        "...",
        "Open source properties, audio filters and signal details.",
    ),
    (
        "Window Capture",
        "Capture an application window in the selected scene.",
    ),
    (
        "Client Area",
        "Capture the window contents without its border or title bar when the backend supports it.",
    ),
    (
        "Refresh Windows",
        "Refresh the window list without changing the source.",
    ),
    (
        "window-source-name",
        "Name this window capture source so you can find it in Sources.",
    ),
    ("Text", "Add an editable text source to the selected scene."),
    ("Bold", "Use a bold text appearance."),
    ("Italic", "Use an italic text appearance."),
    ("Word Wrap", "Wrap text to fit the text box width."),
    (
        "text-source-name",
        "Name this text source so you can find it in Sources.",
    ),
    (
        "text-source-content",
        "Enter up to 4096 characters of text. Line breaks are preserved.",
    ),
    ("Language", "Choose the interface language."),
    ("Display", "Choose the display to capture."),
    ("Audio device", "Choose the audio device to capture."),
    ("Source URL", "Select this source address to copy it."),
    ("Scenes", "Show or hide the Scenes panel."),
    ("Sources", "Show or hide the Sources panel."),
    ("Audio Mixer", "Show or hide the Audio Mixer panel."),
    (
        "Scene Transitions",
        "Show or hide the Scene Transitions panel.",
    ),
    ("Controls", "Show or hide the Controls panel."),
    (
        "Quick Transition",
        "Run this quick transition with its own settings and duration.",
    ),
    ("Add", "Add an item to this list."),
    ("Remove", "Remove the selected item."),
    ("Rename", "Rename the selected item."),
    ("Duplicate", "Create a copy of the selected item."),
    ("Apply", "Apply these settings and close the dialog."),
    (
        "Cancel",
        "Close without saving changes made in this dialog.",
    ),
    ("Close", "Close this panel."),
    ("Delete", "Confirm the removal described in this dialog."),
    (
        "Save",
        "Save changes to the scene collection at the specified path.",
    ),
    ("Open", "Open the specified document."),
    ("Recover", "Open the recovery copy at the specified path."),
    ("Exit", "Close SkyLive. You can save unsaved changes first."),
    (
        "Settings",
        "Open application settings. Only implemented settings pages are available.",
    ),
    ("Save and close", "Save your changes, then close SkyLive."),
    (
        "Keep recovery and close",
        "Keep a recovery copy of your changes, then close.",
    ),
    ("Discard and close", "Close without saving your changes."),
    ("Import OBS", "Import a supported OBS scene collection."),
    (
        "Export OBS",
        "Export the supported scene collection features to OBS format.",
    ),
    ("Import", "Import the file at the specified path."),
    ("Export", "Export to the specified path."),
    (
        "Scene",
        "Choose this scene for Preview. Drag to change its order.",
    ),
    (
        "Source",
        "Select this item to edit its properties. Drag an unlocked row to change its layer order.",
    ),
    (
        "Profile",
        "Switch to this profile’s video and audio settings.",
    ),
    ("Transition preset", "Choose this named transition."),
    (
        "Add Existing",
        "Use an existing source in this scene. Changes to its properties affect every scene that uses it.",
    ),
    (
        "Color Source",
        "Add a solid RGBA color source to the current scene.",
    ),
    ("Image", "Add an image from a file."),
    (
        "Display Capture",
        "Choose a display to capture and whether to include the cursor.",
    ),
    (
        "Audio Input Capture",
        "Add a microphone or another audio input.",
    ),
    (
        "Audio Output Capture",
        "Add system audio or another audio output.",
    ),
    (
        "Capture cursor",
        "Include the pointer in frames captured from this display.",
    ),
    (
        "Refresh Displays",
        "Refresh the list of available displays.",
    ),
    (
        "List Native Devices",
        "Refresh the list of available audio devices.",
    ),
    ("Choose listed device", "Choose an available audio device."),
    (
        "Default device",
        "Follow the system's default device for this audio flow.",
    ),
    (
        "Windows endpoint",
        "Use the exact Windows endpoint identifier entered below.",
    ),
    (
        "macOS device",
        "Use the exact macOS audio DeviceUID entered below.",
    ),
    (
        "macOS system audio",
        "Capture supported system audio through the macOS provider.",
    ),
    (
        "Use device timing",
        "Use the device’s audio timing instead of estimating when sound arrives.",
    ),
    (
        "Exclude this application's audio",
        "Exclude SkyLive's own audio when the selected output provider supports it.",
    ),
    (
        "Properties",
        "Edit the selected item's properties in a dialog.",
    ),
    ("Source Properties", "Edit this audio source’s properties."),
    (
        "Show",
        "Toggle this scene item's visibility and activation.",
    ),
    (
        "Lock",
        "Prevent transform and ordering edits to this scene item.",
    ),
    (
        "Transform",
        "Edit the source position, size, crop, rotation, reflection and opacity.",
    ),
    (
        "Flip Horizontally",
        "Reflect the source horizontally within its transform.",
    ),
    (
        "Flip Vertically",
        "Reflect the source vertically within its transform.",
    ),
    (
        "Move Up",
        "Move the selected item one position toward the top of the list.",
    ),
    (
        "Move Down",
        "Move the selected item one position toward the bottom of the list.",
    ),
    (
        "Volume",
        "Adjust the source volume. Release, press Enter or leave the control to apply.",
    ),
    (
        "Mute",
        "Immediately mute or unmute this shared source without changing its gain.",
    ),
    (
        "Audio Filters",
        "Edit this source’s audio filters. Filters run from top to bottom.",
    ),
    (
        "Add Audio Pan Filter",
        "Add stereo pan controls for the left and right inputs.",
    ),
    (
        "Pan (Left)",
        "Pan the left input from -100 to +100 percent.",
    ),
    (
        "Pan (Right)",
        "Pan the right input from -100 to +100 percent.",
    ),
    (
        "Gain (Left)",
        "Set left input gain from -80 to +30 dB before stereo pan mixing.",
    ),
    (
        "Gain (Right)",
        "Set right input gain from -80 to +30 dB before stereo pan mixing.",
    ),
    (
        "Invert (Left)",
        "Invert the polarity of the left input for this filter.",
    ),
    (
        "Invert (Right)",
        "Invert the polarity of the right input for this filter.",
    ),
    (
        "Move Filter Up",
        "Move this filter up in the list. Filters run from top to bottom.",
    ),
    (
        "Move Filter Down",
        "Move this filter down in the list. Filters run from top to bottom.",
    ),
    ("Remove Filter", "Remove this filter."),
    (
        "Enabled",
        "Turn this module on or off. Its description lists the supported features.",
    ),
    ("Plugins", "Browse modules and turn on available features."),
    (
        "Scene Notes",
        "Edit plain-text notes for this scene. Formatting, linked files, automatic scrolling and hotkeys are not available yet.",
    ),
    (
        "Global Audio Slots",
        "Choose the global desktop and microphone audio sources.",
    ),
    (
        "Add New Audio Source",
        "Create an audio source for this global slot.",
    ),
    (
        "Disabled",
        "Leave this slot or override without an assigned provider.",
    ),
    (
        "Program Audio",
        "View Program audio levels and buffering problems. Speaker monitoring is off.",
    ),
    (
        "Advanced Audio Properties",
        "Review audio source activity, volume, mute and filter settings.",
    ),
    (
        "General",
        "Choose the interface language, reload translations, and configure projectors and Multiview.",
    ),
    (
        "Appearance",
        "Choose the interface theme and scale.",
    ),
    (
        "Stream",
        "Configure the streaming service, server and stream key.",
    ),
    (
        "Output",
        "Configure recording encoders, audio tracks and the replay buffer.",
    ),
    (
        "Audio",
        "Edit the selected profile's audio format and the user's global buffering preference.",
    ),
    (
        "Video",
        "Edit the selected profile's canvas size and frame-rate format.",
    ),
    ("Hotkeys", "Configure local and Windows global keyboard shortcuts."),
    (
        "Accessibility",
        "Choose selection, crop, hover and audio meter colors.",
    ),
    (
        "Advanced",
        "Configure process priority and stream reconnection.",
    ),
    (
        "Low Latency Audio Buffering",
        "Reduce audio buffering for lower latency. Turn this off if audio becomes unreliable.",
    ),
    (
        "Reload localization CSV",
        "Reload the edited translation file. The current language remains available if the file contains errors.",
    ),
    (
        "Studio Mode",
        "Prepare scenes in Preview before sending them to Program.",
    ),
    (
        "Transition",
        "Send Preview to Program using the selected transition.",
    ),
    ("Quick Cut", "Send Preview to Program immediately."),
    (
        "Cut",
        "Switch directly between scenes without a timed blend.",
    ),
    ("Fade", "Blend between scenes over the configured duration."),
    ("Swipe", "Move between scenes using a directional wipe."),
    ("Luma Wipe", "Reveal the incoming scene using a grayscale pattern and an adjustable soft edge."),
    (
        "Slide",
        "Slide one scene over the other in the selected direction.",
    ),
    (
        "Fade to Color",
        "Fade through the chosen color while changing scenes.",
    ),
    (
        "Fade To Color",
        "Fade through the chosen color while changing scenes.",
    ),
    ("Left", "Set the directional transition toward the left."),
    ("Right", "Set the directional transition toward the right."),
    ("Up", "Set the directional transition upward."),
    ("Down", "Set the directional transition downward."),
    (
        "Swipe In",
        "Choose how the incoming scene is revealed during Swipe.",
    ),
    (
        "Switch Point",
        "Choose the percentage at which Fade to Color changes from the old scene to the new one.",
    ),
    (
        "Duration (ms)",
        "Set the transition length in milliseconds, from 50 to 20000.",
    ),
    ("Apply Duration", "Apply the transition duration."),
    (
        "T-bar",
        "Drag to blend Preview into Program. Release near an end to cancel or finish; release in the middle to hold.",
    ),
    (
        "Release T-bar",
        "Release the current position to cancel, hold or finish the transition.",
    ),
    (
        "Preset Settings",
        "Create, edit or remove named transitions.",
    ),
    ("Preset Library", "Browse your named transitions."),
    (
        "Preset Properties",
        "Edit this named transition. Changes affect every place that uses it.",
    ),
    (
        "Add Transition Preset",
        "Create a named transition. You can select it after adding it.",
    ),
    (
        "Remove Preset",
        "Review what uses this transition before deleting it.",
    ),
    (
        "Transition Override",
        "Choose a different transition for this scene.",
    ),
    (
        "Transition Override…",
        "Choose a different transition for this scene.",
    ),
    (
        "Override global transition for this scene",
        "Use a separate transition for this scene. Turning this off remembers its duration.",
    ),
    (
        "Add Quick Transition",
        "Add a quick transition button with its own transition and duration.",
    ),
    (
        "Edit Quick Transition",
        "Edit this quick transition button.",
    ),
    (
        "Remove Quick Transition",
        "Remove this quick transition button.",
    ),
    (
        "Fade To Black",
        "Fade to black and keep the prepared scene ready for the return transition.",
    ),
    ("Options", "Open actions for this item."),
    (
        "Stinger / other providers",
        "Additional transition providers are not implemented yet.",
    ),
    (
        "No providers available",
        "This menu has no implemented dynamic provider.",
    ),
    ("name", "Enter the item’s name."),
    ("profile-name", "Enter a name for this profile."),
    ("preset-name", "Enter a unique name for this transition."),
    ("image-name", "Enter a name for the image source."),
    ("image-file", "Enter the image file path."),
    (
        "display-source-name",
        "Enter a name for the display capture source.",
    ),
    ("audio-source-name", "Enter a name for the audio source."),
    (
        "audio-target-identity",
        "Enter the exact device ID or DeviceUID, rather than its display name.",
    ),
    (
        "obs-path",
        "Enter the OBS collection file path. Native documents use a separate path.",
    ),
    (
        "plugin-search",
        "Search module names, IDs, descriptions and supported features.",
    ),
    (
        "scene-notes-text",
        "Enter this scene’s note, up to 16384 UTF-8 bytes.",
    ),
    ("Color (RGBA)", "Choose the source color and opacity."),
    ("Color", "Choose a color."),
    ("Rotation", "Enter clockwise rotation in degrees."),
    ("Opacity", "Set source opacity from 0 to 1."),
    ("Text field", "Enter a value for this field."),
    (
        "Numeric value",
        "Enter a number. See the label for its units and valid range.",
    ),
    ("Slider", "Adjust this value with the pointer or keyboard."),
    ("Menu", "Open this menu to inspect its available actions."),
    ("Item", "Select this item."),
];

pub fn text(ui: &egui::Ui, caption: impl AsRef<str>) -> String {
    let caption = caption.as_ref();
    let english = HINTS
        .iter()
        .find_map(|(key, hint)| (*key == caption).then_some(*hint))
        .or_else(|| {
            ACTION_HINTS
                .iter()
                .find_map(|(key, hint)| (*key == caption).then_some(*hint))
        })
        .unwrap_or("Select this item.");
    crate::i18n::tr(ui, english)
}

const ACTION_HINTS: &[(&str, &str)] = &[
    (
        "actionCopySource",
        "Copy the selected source for a later paste.",
    ),
    (
        "actionPasteRef",
        "Paste another reference to the copied source, sharing its properties.",
    ),
    (
        "actionCopyFilters",
        "Copy filter settings from the selected source.",
    ),
    (
        "actionPasteFilters",
        "Paste copied filter settings onto the selected source when supported.",
    ),
    (
        "resetUI",
        "Restore the default dock layout without changing scene content.",
    ),
    (
        "actionFullscreenInterface",
        "Toggle the interface fullscreen mode when supported.",
    ),
    (
        "actionAddScene",
        "Add a new scene to the current collection.",
    ),
    (
        "actionAddSource",
        "Choose a source type to add to the current scene.",
    ),
    (
        "actionRemoveScene",
        "Remove the selected scene while retaining at least one scene.",
    ),
    (
        "actionRemoveSource",
        "Remove the selected scene item and prune an unreferenced source.",
    ),
    (
        "actionSourceProperties",
        "Edit the properties shared by every reference to the selected source.",
    ),
    (
        "actionSceneUp",
        "Move the selected item one position upward in its list.",
    ),
    (
        "actionSourceUp",
        "Move the selected item one position upward in its list.",
    ),
    (
        "actionSceneDown",
        "Move the selected item one position downward in its list.",
    ),
    (
        "actionSourceDown",
        "Move the selected item one position downward in its list.",
    ),
    (
        "actionShow_Recordings",
        "Open the recordings location when the recording feature is available.",
    ),
    (
        "actionRemux",
        "Convert supported finalized SkyLive MKV recordings to MP4 without re-encoding or overwriting files.",
    ),
    (
        "action_Settings",
        "Open General settings and the available settings pages.",
    ),
    (
        "actionE_xit",
        "Resolve unsaved document changes and close SkyLive.",
    ),
    (
        "actionShowLogs",
        "Inspect the application log when log viewing is implemented.",
    ),
    (
        "actionUploadLastLog",
        "Upload the selected diagnostic log only through an available upload action.",
    ),
    (
        "actionUploadCurrentLog",
        "Upload the selected diagnostic log only through an available upload action.",
    ),
    (
        "actionViewCurrentLog",
        "Inspect the application log when log viewing is implemented.",
    ),
    (
        "actionUndo",
        "Undo the last scene collection change. Profiles and preferences are kept.",
    ),
    (
        "actionRedo",
        "Redo the last undone scene collection change. Profiles and preferences are kept.",
    ),
    (
        "actionEditTransform",
        "Edit position, size, crop, rotation, reflection and opacity for the selected video item.",
    ),
    (
        "actionCopyTransform",
        "Copy the selected video item transform when this action is implemented.",
    ),
    (
        "actionPasteTransform",
        "Paste a copied transform onto the selected video item when supported.",
    ),
    (
        "actionRotate90CW",
        "Rotate the selected video item 90 degrees clockwise, preserving OBS top-left bounds.",
    ),
    (
        "actionRotate90CCW",
        "Rotate the selected video item 90 degrees counterclockwise, preserving OBS top-left bounds.",
    ),
    (
        "actionRotate180",
        "Rotate the selected video item 180 degrees.",
    ),
    (
        "actionFitToScreen",
        "Fit the selected video item inside the canvas while preserving its aspect ratio.",
    ),
    (
        "actionStretchToScreen",
        "Stretch the selected video item to fill the canvas.",
    ),
    (
        "actionResetTransform",
        "Reset the selected video item to its native size and default transform.",
    ),
    (
        "actionCenterToScreen",
        "Center the selected video item horizontally and vertically in the canvas.",
    ),
    (
        "actionVerticalCenter",
        "Center the selected video item vertically in the canvas.",
    ),
    (
        "actionHorizontalCenter",
        "Center the selected video item horizontally in the canvas.",
    ),
    (
        "actionFlipHorizontal",
        "Reflect the selected video item horizontally.",
    ),
    (
        "actionFlipVertical",
        "Reflect the selected video item vertically.",
    ),
    (
        "actionMoveUp",
        "Move the selected item one position upward in its list.",
    ),
    (
        "actionMoveDown",
        "Move the selected item one position downward in its list.",
    ),
    (
        "actionMoveToTop",
        "Move the selected item to the top of its list.",
    ),
    (
        "actionMoveToBottom",
        "Move the selected item to the bottom of its list.",
    ),
    (
        "actionCheckForUpdates",
        "Check for an application update when an updater is implemented.",
    ),
    (
        "actionShowMacPermissions",
        "Review the native capture permissions when this integration is available.",
    ),
    (
        "actionRepair",
        "Check installed files when installation integrity checks are implemented.",
    ),
    (
        "actionRestartSafe",
        "Restart with optional integrations disabled when safe restart is available.",
    ),
    (
        "actionReleaseNotes",
        "Open the reference OBS Studio 32.2.2 release notes in your default browser.",
    ),
    (
        "actionInteract",
        "Open Browser Interaction for the selected Browser Source, with real page input and refresh.",
    ),
    (
        "actionAdvAudioProperties",
        "Open advanced audio routing and physical monitoring controls.",
    ),
    (
        "actionWebsite",
        "Open the OBS Studio website in your default browser.",
    ),
    (
        "actionNewSceneCollection",
        "Create an empty scene collection. Profiles and preferences are kept.",
    ),
    (
        "actionDupSceneCollection",
        "Duplicate the current collection when this action is implemented.",
    ),
    (
        "actionRenameSceneCollection",
        "Change the current scene collection display name.",
    ),
    (
        "actionRemoveSceneCollection",
        "Remove a scene collection when collection management is implemented.",
    ),
    (
        "actionImportSceneCollection",
        "Import a supported OBS scene collection.",
    ),
    (
        "actionExportSceneCollection",
        "Export supported scene collection features to OBS format.",
    ),
    (
        "actionShowMissingFiles",
        "Inspect missing file references and stage explicit validated replacements without changing source identities.",
    ),
    (
        "actionRemigrateSceneCollection",
        "Reset imported base-resolution metadata when migration support is implemented.",
    ),
    (
        "actionNewProfile",
        "Create a profile with default settings. Your scenes are kept.",
    ),
    (
        "actionDupProfile",
        "Copy the selected profile under a new name.",
    ),
    ("actionRenameProfile", "Rename the selected profile."),
    (
        "actionRemoveProfile",
        "Review removal of the selected profile; the last profile cannot be removed.",
    ),
    (
        "actionImportProfile",
        "Import a profile when profile interchange is implemented.",
    ),
    (
        "actionExportProfile",
        "Export the selected profile when profile interchange is implemented.",
    ),
    (
        "actionShowSettingsFolder",
        "Open the application settings directory when that platform integration is available.",
    ),
    (
        "actionShowProfileFolder",
        "Open the selected profile directory when directory-based profiles are implemented.",
    ),
    (
        "actionAlwaysOnTop",
        "Keep this application window above other windows.",
    ),
    (
        "toggleListboxToolbars",
        "Show or hide the Scenes and Sources dock toolbars. Right-click a dock heading or item to keep using its actions.",
    ),
    (
        "toggleStatusBar",
        "Show or hide the application status bar.",
    ),
    (
        "actionLockPreview",
        "Lock or unlock source editing in Preview.",
    ),
    (
        "actionScaleWindow",
        "Fit the preview display to its available window area.",
    ),
    (
        "actionScaleCanvas",
        "Show Preview at the canvas pixel scale.",
    ),
    (
        "actionScaleOutput",
        "Show Preview at the current output pixel scale.",
    ),
    ("actionPasteDup", "Paste an independent copy of the source."),
    (
        "idianPlayground",
        "Open the optional Idian provider when it has a reviewed implementation.",
    ),
    (
        "autoConfigure2",
        "Run automatic configuration only when the configuration workflow is implemented.",
    ),
    (
        "autoConfigure",
        "Open supported manual setup. Automatic optimization tests are not implemented.",
    ),
    (
        "actionOpenPluginManager",
        "Browse modules and turn on available features.",
    ),
    (
        "stats",
        "Open runtime statistics when that statistics view is implemented.",
    ),
    (
        "resetDocks",
        "Restore the default dock layout without changing scene content.",
    ),
    (
        "lockDocks",
        "Lock dock rearrangement when movable docks are implemented.",
    ),
    (
        "sideDocks",
        "Use full-height side docks when that layout mode is implemented.",
    ),
    (
        "actionHelpPortal",
        "Open the OBS Studio help portal in your default browser.",
    ),
    (
        "actionShowCrashLogs",
        "Inspect recorded crash reports when crash reporting is implemented.",
    ),
    (
        "actionUploadLastCrashLog",
        "Upload a crash report only through an available upload action.",
    ),
    (
        "actionDiscord",
        "Open the OBS Studio Discord community in your default browser.",
    ),
    (
        "actionShowWhatsNew",
        "View release information when this integration is available.",
    ),
    (
        "actionShowAbout",
        "Show the application version, license and available features.",
    ),
    (
        "toggleSourceIcons",
        "Show or hide source-type icons in the Sources list.",
    ),
    (
        "toggleContextToolbars",
        "Show or hide the selected source toolbar with properties, filters and media playback controls.",
    ),
    (
        "toggleContextBar",
        "Show or hide the selected source toolbar with properties, filters and media playback controls.",
    ),
    (
        "actionMainUndo",
        "Undo the last scene collection change. Profiles and preferences are kept.",
    ),
    (
        "actionMainRedo",
        "Redo the last undone scene collection change. Profiles and preferences are kept.",
    ),
    (
        "actionMixerToolbarAdvAudio",
        "Open advanced audio routing and physical monitoring controls.",
    ),
    (
        "actionSceneFilters",
        "Open the current scene video filter stack.",
    ),
    (
        "actionSceneGridMode",
        "Display scenes as a grid of selectable tiles.",
    ),
    (
        "actionSceneListMode",
        "Display scenes as a compact list.",
    ),
    (
        "actionPreview",
        "Toggle the preview display when this visibility action is implemented.",
    ),
    (
        "actionPreviewZoomIn",
        "Zoom in on the Preview display without changing output resolution.",
    ),
    (
        "actionPreviewZoomOut",
        "Zoom out on the Preview display without changing output resolution.",
    ),
    (
        "actionPreviewResetZoom",
        "Reset Preview zoom and pan for this window.",
    ),
];
