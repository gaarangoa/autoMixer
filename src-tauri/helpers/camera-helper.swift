// AutoMixer native camera helper.
//
// WHY THIS EXISTS: ffmpeg's avfoundation input can only request UNCOMPRESSED
// pixel formats. USB 2.0 webcams (e.g. Logitech C920) can't push raw 1080p over
// the bus faster than ~5fps — their 30fps modes exist only via in-camera MJPEG
// compression, a format ffmpeg never selects. AVFoundation used directly (like
// every browser does) negotiates those compressed formats transparently, so this
// tiny helper owns the camera and gives us:
//   • full native resolution at real 30fps,
//   • an MJPEG preview stream on stdout (multipart, ffmpeg-compatible framing),
//   • optional H.264 recording to a file (hardware encoder via AVAssetWriter),
// in ONE process per camera — preview keeps running while recording.
//
// Usage: camera-helper --device "<name>" [--record /path/out.mp4] [--preview]
// stderr protocol (line-oriented):
//   size=<W>x<H> fps=<F>   once, after format negotiation
//   ready                  first frame captured
//   t=<ms>                 media clock while recording (every ~200ms)
//   error: <message>       fatal
// SIGINT finalizes the recording file cleanly and exits 0.

import AVFoundation
import CoreImage
import Foundation

func die(_ message: String) -> Never {
    FileHandle.standardError.write("error: \(message)\n".data(using: .utf8)!)
    exit(1)
}

func logErr(_ line: String) {
    FileHandle.standardError.write((line + "\n").data(using: .utf8)!)
}

// --- Argument parsing --------------------------------------------------------
var deviceName = ""
var recordPath: String?
var previewOn = false
// 0 = unlimited (full native). Otherwise pick the best format whose width fits —
// used to step DOWN under USB bandwidth pressure (several cameras on one bus).
var maxWidth = 0
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    switch a {
    case "--device": deviceName = args.isEmpty ? "" : args.removeFirst()
    case "--record": recordPath = args.isEmpty ? nil : args.removeFirst()
    case "--preview": previewOn = true
    case "--max-width": maxWidth = args.isEmpty ? 0 : Int(args.removeFirst()) ?? 0
    default: die("unknown arg \(a)")
    }
}
if deviceName.isEmpty { die("--device required") }

// --- Device + format selection ------------------------------------------------
var types: [AVCaptureDevice.DeviceType] = [.builtInWideAngleCamera, .external]
if #available(macOS 14.0, *) {
    types.append(.continuityCamera)
    types.append(.deskViewCamera)
}
let discovery = AVCaptureDevice.DiscoverySession(deviceTypes: types, mediaType: .video, position: .unspecified)
let wanted = deviceName.lowercased()
guard let device = discovery.devices.first(where: {
    let n = $0.localizedName.lowercased()
    return n == wanted || n.contains(wanted) || wanted.contains(n)
}) else {
    die("camera not found: \(deviceName) (available: \(discovery.devices.map { $0.localizedName }.joined(separator: ", ")))")
}

// Largest-area format that can sustain >=25fps (this is where AVFoundation shines:
// it happily picks the camera's compressed format and decompresses internally).
func area(_ f: AVCaptureDevice.Format) -> Int {
    let d = CMVideoFormatDescriptionGetDimensions(f.formatDescription)
    return Int(d.width) * Int(d.height)
}
func width(_ f: AVCaptureDevice.Format) -> Int {
    Int(CMVideoFormatDescriptionGetDimensions(f.formatDescription).width)
}
let fastFormats = device.formats.filter { f in
    f.videoSupportedFrameRateRanges.contains { $0.maxFrameRate >= 25 }
}
var pool = fastFormats.isEmpty ? device.formats : fastFormats
if maxWidth > 0 {
    let capped = pool.filter { width($0) <= maxWidth }
    if !capped.isEmpty { pool = capped }
}
guard let format = pool.max(by: { area($0) < area($1) }) else {
    die("no capture formats on \(deviceName)")
}
let dims = CMVideoFormatDescriptionGetDimensions(format.formatDescription)
let maxFps = format.videoSupportedFrameRateRanges.map { $0.maxFrameRate }.max() ?? 30
let fps = min(30.0, maxFps)

logErr("size=\(dims.width)x\(dims.height) fps=\(Int(fps.rounded()))")

// --- Session -------------------------------------------------------------------
// macOS quirk: there is no .inputPriority preset — instead, setting activeFormat
// AFTER the input joins the session makes the session honor it.
let session = AVCaptureSession()
session.beginConfiguration()
guard let input = try? AVCaptureDeviceInput(device: device), session.canAddInput(input) else {
    die("could not open \(deviceName) as input")
}
session.addInput(input)
// Configure with retries: right after a previous owner of this camera exits,
// macOS can take a moment to release it ("Cannot Use <device>"). Back off and
// retry instead of dying on the first attempt.
var configured = false
var lastConfigError = ""
for _ in 0..<8 {
    do {
        try device.lockForConfiguration()
        device.activeFormat = format
        // Frame duration must come from the format's OWN advertised ranges — building
        // one from a rounded fps (e.g. 1/30 vs the device's exact 30.000030) throws
        // NSInvalidArgumentException. If the range allows faster than ~31fps, cap to a
        // plain 30 (inside the range, so it's valid); otherwise lock to the range's max
        // rate using its exact CMTime.
        if let range = format.videoSupportedFrameRateRanges.max(by: { $0.maxFrameRate < $1.maxFrameRate }) {
            let duration = range.maxFrameRate > 31.0 ? CMTimeMake(value: 1, timescale: 30) : range.minFrameDuration
            device.activeVideoMinFrameDuration = duration
            device.activeVideoMaxFrameDuration = duration
        }
        device.unlockForConfiguration()
        configured = true
        break
    } catch {
        lastConfigError = error.localizedDescription
        Thread.sleep(forTimeInterval: 0.5)
    }
}
if !configured {
    die("could not configure \(deviceName): \(lastConfigError)")
}
let output = AVCaptureVideoDataOutput()
output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
output.alwaysDiscardsLateVideoFrames = true

// --- Writer (optional recording) ------------------------------------------------
var writer: AVAssetWriter?
var writerInput: AVAssetWriterInput?
if let path = recordPath {
    let url = URL(fileURLWithPath: path)
    try? FileManager.default.removeItem(at: url)
    let pixels = Int(dims.width) * Int(dims.height)
    let bitrate: Int = pixels >= 3840 * 2160 ? 45_000_000
        : pixels >= 2560 * 1440 ? 28_000_000
        : pixels >= 1920 * 1080 ? 16_000_000
        : 8_000_000
    guard let w = try? AVAssetWriter(outputURL: url, fileType: .mp4) else { die("cannot create writer at \(path)") }
    let settings: [String: Any] = [
        AVVideoCodecKey: AVVideoCodecType.h264,
        AVVideoWidthKey: Int(dims.width),
        AVVideoHeightKey: Int(dims.height),
        AVVideoCompressionPropertiesKey: [
            AVVideoAverageBitRateKey: bitrate,
            AVVideoMaxKeyFrameIntervalKey: 60,
            AVVideoExpectedSourceFrameRateKey: Int(fps.rounded()),
        ],
    ]
    let wi = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
    wi.expectsMediaDataInRealTime = true
    guard w.canAdd(wi) else { die("writer rejected input") }
    w.add(wi)
    writer = w
    writerInput = wi
}

// --- Frame delegate --------------------------------------------------------------
// The capture callback must stay CHEAP or the camera drops frames and the
// recording loses fps. So the callback only appends to the writer and parks the
// newest pixel buffer in a slot; a separate preview thread JPEG-encodes at its
// own pace (latest frame wins, older ones are simply skipped).
final class Delegate: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    let previewOn: Bool
    let writer: AVAssetWriter?
    let writerInput: AVAssetWriterInput?
    var firstPTS: CMTime?
    var announcedReady = false
    var lastClockPrint = Date.distantPast
    let slotLock = NSLock()
    var previewSlot: CVPixelBuffer?

    init(previewOn: Bool, writer: AVAssetWriter?, writerInput: AVAssetWriterInput?) {
        self.previewOn = previewOn
        self.writer = writer
        self.writerInput = writerInput
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        if firstPTS == nil {
            firstPTS = pts
            if let w = writer {
                w.startWriting()
                w.startSession(atSourceTime: pts)
            }
        }
        if !announcedReady {
            announcedReady = true
            logErr("ready")
        }
        if let w = writer, let wi = writerInput, w.status == .writing, wi.isReadyForMoreMediaData {
            wi.append(sampleBuffer)
            // Media clock for transport alignment (throttled).
            let now = Date()
            if now.timeIntervalSince(lastClockPrint) >= 0.2, let first = firstPTS {
                lastClockPrint = now
                let ms = Int(CMTimeGetSeconds(CMTimeSubtract(pts, first)) * 1000)
                logErr("t=\(ms)")
            }
        }
        if previewOn, let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) {
            slotLock.lock()
            previewSlot = pixelBuffer
            slotLock.unlock()
        }
    }

    func takePreviewFrame() -> CVPixelBuffer? {
        slotLock.lock()
        defer { slotLock.unlock() }
        let buffer = previewSlot
        previewSlot = nil
        return buffer
    }
}

/// Preview encoder loop: ~20fps, entirely off the capture path.
func startPreviewLoop(_ delegate: Delegate) {
    let ciContext = CIContext()
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    let stdout = FileHandle.standardOutput
    let previewMaxWidth: CGFloat = 1280
    Thread.detachNewThread {
        while true {
            Thread.sleep(forTimeInterval: 0.05)
            guard let pixelBuffer = delegate.takePreviewFrame() else { continue }
            var image = CIImage(cvPixelBuffer: pixelBuffer)
            let width = image.extent.width
            if width > previewMaxWidth {
                let scale = previewMaxWidth / width
                image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
            }
            guard let jpeg = ciContext.jpegRepresentation(of: image, colorSpace: colorSpace, options: [
                kCGImageDestinationLossyCompressionQuality as CIImageRepresentationOption: 0.7,
            ]) else { continue }
            // ffmpeg-compatible mpjpeg part framing (boundary "ffmpeg") so the
            // existing control-server route serves it unchanged.
            var part = Data()
            part.append("--ffmpeg\r\nContent-type: image/jpeg\r\nContent-length: \(jpeg.count)\r\n\r\n".data(using: .utf8)!)
            part.append(jpeg)
            part.append("\r\n".data(using: .utf8)!)
            stdout.write(part)
        }
    }
}

let delegate = Delegate(previewOn: previewOn, writer: writer, writerInput: writerInput)
if previewOn { startPreviewLoop(delegate) }
let queue = DispatchQueue(label: "camera-frames")
output.setSampleBufferDelegate(delegate, queue: queue)
guard session.canAddOutput(output) else { die("cannot add video output") }
session.addOutput(output)
session.commitConfiguration()
session.startRunning()

// Frame watchdog: if the session runs but the device never delivers a frame
// (still mid-release by a previous owner), exit with a distinct message so the
// supervisor can respawn us instead of waiting forever.
DispatchQueue.main.asyncAfter(deadline: .now() + 6.0) {
    if !delegate.announcedReady {
        die("no frames from \(deviceName) after 6s")
    }
}

// --- Clean shutdown on SIGINT/SIGTERM: finalize the mp4 then exit. ----------------
func shutdown() {
    session.stopRunning()
    if let w = writer, w.status == .writing {
        writerInput?.markAsFinished()
        let sema = DispatchSemaphore(value: 0)
        w.finishWriting { sema.signal() }
        _ = sema.wait(timeout: .now() + 10)
    }
    exit(0)
}
signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)
let sigint = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
sigint.setEventHandler { shutdown() }
sigint.resume()
let sigterm = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
sigterm.setEventHandler { shutdown() }
sigterm.resume()

RunLoop.main.run()
