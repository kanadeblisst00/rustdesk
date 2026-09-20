#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#include <unistd.h>
#include <algorithm>
#include <cmath>

static NSString *sessionIdentity() {
    if (getuid() == 0 || getuid() != geteuid()) return nil;
    NSDictionary *session = CFBridgingRelease(CGSessionCopyCurrentDictionary());
    if (!session || ![session[(__bridge NSString *)kCGSessionOnConsoleKey] boolValue] ||
        ![session[(__bridge NSString *)kCGSessionLoginDoneKey] boolValue] ||
        [session[(__bridge NSString *)kCGSessionUserIDKey] unsignedIntValue] != getuid() ||
        [session[@"CGSSessionScreenIsLocked"] boolValue] || IsSecureEventInputEnabled()) return nil;
    NSNumber *console = session[(__bridge NSString *)kCGSessionConsoleSetKey];
    if (!console) return nil;
    return [NSString stringWithFormat:@"macos:%u:%@", getuid(), console];
}

static char *jsonString(NSDictionary *value) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:value options:0 error:nil];
    if (!data) return nullptr;
    NSString *string = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return strdup(string.UTF8String);
}

extern "C" char *mcp_local_mac_snapshot() {
    @autoreleasepool {
        if (@available(macOS 14.0, *)) {
            NSString *identity = sessionIdentity();
            if (!identity) return jsonString(@{@"error":@"Local macOS desktop is locked, inactive, using secure input, or running as another user"});
            CGDirectDisplayID ids[64];
            uint32_t count = 0;
            if (CGGetActiveDisplayList(64, ids, &count) != kCGErrorSuccess || count == 0)
                return jsonString(@{@"error":@"Local macOS displays unavailable"});
            NSMutableArray *displays = [NSMutableArray array];
            for (uint32_t i = 0; i < count; ++i) {
                CGRect bounds = CGDisplayBounds(ids[i]);
                CGDisplayModeRef mode = CGDisplayCopyDisplayMode(ids[i]);
                if (!mode) return jsonString(@{@"error":@"Cannot read local display mode"});
                size_t width = CGDisplayModeGetPixelWidth(mode), height = CGDisplayModeGetPixelHeight(mode);
                double rotation = CGDisplayRotation(ids[i]);
                CGDisplayModeRelease(mode);
                // Some modes already report rotated dimensions. Match the current desktop bounds.
                if (std::abs(width * bounds.size.height - height * bounds.size.width) >
                    std::abs(height * bounds.size.height - width * bounds.size.width)) std::swap(width, height);
                [displays addObject:@{@"id":@(ids[i]), @"name":[NSString stringWithFormat:@"display-%u", ids[i]],
                    @"x":@(bounds.origin.x), @"y":@(bounds.origin.y), @"width":@(width), @"height":@(height),
                    @"logical_width":@(bounds.size.width), @"logical_height":@(bounds.size.height),
                    @"rotation":@(rotation), @"primary":@(CGDisplayIsMain(ids[i]) != 0)}];
            }
            [displays sortUsingComparator:^NSComparisonResult(NSDictionary *a, NSDictionary *b) {
                if ([a[@"primary"] boolValue] != [b[@"primary"] boolValue]) return [a[@"primary"] boolValue] ? NSOrderedAscending : NSOrderedDescending;
                return [a[@"id"] compare:b[@"id"]];
            }];
            pid_t pid = NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier;
            NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
            NSString *active = @"";
            for (NSDictionary *window in windows) {
                if ([window[(__bridge NSString *)kCGWindowOwnerPID] intValue] == pid &&
                    [window[(__bridge NSString *)kCGWindowLayer] intValue] == 0) {
                    active = [NSString stringWithFormat:@"%d:%@", pid, window[(__bridge NSString *)kCGWindowNumber]];
                    break;
                }
            }
            return jsonString(@{@"desktop_id":identity, @"active_window":active, @"displays":displays,
                @"capture_allowed":@(CGPreflightScreenCaptureAccess()),
                @"input_allowed":@(AXIsProcessTrusted() && CGPreflightPostEventAccess()),
                @"provider":@"macOS ScreenCaptureKit / CGEvent (physical pixels mapped to points)"});
        }
        return jsonString(@{@"error":@"Local desktop capture requires macOS 14 or newer"});
    }
}

extern "C" int mcp_local_mac_capture(uint32_t id, uint32_t width, uint32_t height, uint8_t *rgba) {
    @autoreleasepool {
        if (@available(macOS 14.0, *)) {
            if (!rgba || !width || !height || (uint64_t)width * height > 16 * 1024 * 1024 ||
                !sessionIdentity() || !CGPreflightScreenCaptureAccess()) return 1;
            // Blocks may outlive a timeout. Their state is heap retained; no caller buffer is captured.
            NSLock *lock = [[NSLock alloc] init];
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            __block SCDisplay *display = nil;
            __block bool expired = false;
            [SCShareableContent getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:YES completionHandler:^(SCShareableContent *content, NSError *error) {
                [lock lock];
                if (!expired && !error) for (SCDisplay *candidate in content.displays) {
                    if (candidate.displayID == id) { display = candidate; break; }
                }
                [lock unlock];
                dispatch_semaphore_signal(ready);
            }];
            long waited = dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC));
            [lock lock];
            expired = true;
            SCDisplay *selected = display;
            [lock unlock];
            if (waited != 0 || !selected) return 2;
            SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:selected excludingWindows:@[]];
            SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
            config.width = width;
            config.height = height;
            config.showsCursor = YES;
            config.scalesToFit = YES;
            dispatch_semaphore_t captured = dispatch_semaphore_create(0);
            NSLock *imageLock = [[NSLock alloc] init];
            __block CGImageRef result = nullptr;
            __block bool imageExpired = false;
            [SCScreenshotManager captureImageWithFilter:filter configuration:config completionHandler:^(CGImageRef image, NSError *error) {
                [imageLock lock];
                if (!imageExpired && image && !error) result = CGImageRetain(image);
                [imageLock unlock];
                dispatch_semaphore_signal(captured);
            }];
            waited = dispatch_semaphore_wait(captured, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC));
            [imageLock lock];
            imageExpired = true;
            CGImageRef image = result;
            [imageLock unlock];
            if (waited != 0 || !image) { if (image) CGImageRelease(image); return 3; }
            if (CGImageGetWidth(image) != width || CGImageGetHeight(image) != height) { CGImageRelease(image); return 4; }
            CGColorSpaceRef color = CGColorSpaceCreateDeviceRGB();
            CGContextRef context = CGBitmapContextCreate(rgba, width, height, 8, (size_t)width * 4, color,
                kCGBitmapByteOrder32Big | kCGImageAlphaPremultipliedLast);
            CGColorSpaceRelease(color);
            if (!context) { CGImageRelease(image); return 5; }
            CGContextDrawImage(context, CGRectMake(0, 0, width, height), image);
            CGContextRelease(context);
            CGImageRelease(image);
            return 0;
        }
        return 6;
    }
}

extern "C" bool mcp_local_mac_idle() {
    for (CGKeyCode code = 0; code < 128; ++code) if (CGEventSourceKeyState(kCGEventSourceStateCombinedSessionState, code)) return false;
    for (CGMouseButton button : {kCGMouseButtonLeft, kCGMouseButtonRight, kCGMouseButtonCenter})
        if (CGEventSourceButtonState(kCGEventSourceStateCombinedSessionState, button)) return false;
    return true;
}

extern "C" bool mcp_local_mac_event(uint32_t kind, uint16_t code, bool down, double x, double y,
    uint64_t flags, uint32_t clicks, const uint16_t *text, size_t length) {
    @autoreleasepool {
        // Releases remain possible after a lock/cancel so our own held inputs can be unwound.
        bool release = (kind == 1 || kind == 2) && !down;
        if (!release && (!sessionIdentity() || !AXIsProcessTrusted() || !CGPreflightPostEventAccess())) return false;
        CGEventSourceRef source = CGEventSourceCreate(kCGEventSourceStatePrivate);
        if (!source) return false;
        CGEventRef event = nullptr;
        if (kind == 0 || kind == 1) {
            CGMouseButton button = code == 3 ? kCGMouseButtonRight : code == 2 ? kCGMouseButtonCenter : kCGMouseButtonLeft;
            CGEventType type;
            if (kind == 0) type = code == 1 ? kCGEventLeftMouseDragged : code == 3 ? kCGEventRightMouseDragged : code == 2 ? kCGEventOtherMouseDragged : kCGEventMouseMoved;
            else type = code == 1 ? (down ? kCGEventLeftMouseDown : kCGEventLeftMouseUp) : code == 3 ? (down ? kCGEventRightMouseDown : kCGEventRightMouseUp) : (down ? kCGEventOtherMouseDown : kCGEventOtherMouseUp);
            event = CGEventCreateMouseEvent(source, type, CGPointMake(x, y), button);
            if (event && kind == 1) CGEventSetIntegerValueField(event, kCGMouseEventClickState, clicks);
        } else if (kind == 2) {
            event = CGEventCreateKeyboardEvent(source, code, down);
        } else if (kind == 3 && text && length > 0 && length <= 2) {
            event = CGEventCreateKeyboardEvent(source, 0, true);
            CGEventRef up = CGEventCreateKeyboardEvent(source, 0, false);
            if (!event || !up) { if (event) CFRelease(event); if (up) CFRelease(up); CFRelease(source); return false; }
            CGEventKeyboardSetUnicodeString(event, length, text);
            CGEventKeyboardSetUnicodeString(up, length, text);
            CGEventSetFlags(event, 0);
            CGEventSetFlags(up, 0);
            CGEventPost(kCGHIDEventTap, event);
            CGEventPost(kCGHIDEventTap, up);
            CFRelease(up);
            CFRelease(event);
            CFRelease(source);
            return true;
        } else if (kind == 4) {
            event = CGEventCreateScrollWheelEvent(source, kCGScrollEventUnitLine, 2, (int32_t)y, (int32_t)x);
        }
        CFRelease(source);
        if (!event) return false;
        CGEventSetFlags(event, (CGEventFlags)flags);
        CGEventPost(kCGHIDEventTap, event);
        CFRelease(event);
        return true;
    }
}
