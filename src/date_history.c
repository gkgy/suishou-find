// Read only the historical part of the host's FSEvents stream on a worker thread.
#include <CoreServices/CoreServices.h>
#include <stdbool.h>
#include <stdint.h>

typedef bool (*PathSink)(void *, const char *);
typedef struct {
    void *sink_context;
    PathSink sink;
    bool done;
    bool gap;
} Replay;

static void receive_history(ConstFSEventStreamRef stream, void *context, size_t count,
                            void *raw_paths, const FSEventStreamEventFlags flags[],
                            const FSEventStreamEventId ids[]) {
    (void)stream;
    (void)ids;
    Replay *replay = context;
    char **paths = raw_paths;
    for (size_t i = 0; i < count && !replay->done; ++i) {
        if (flags[i] & (kFSEventStreamEventFlagMustScanSubDirs |
                        kFSEventStreamEventFlagEventIdsWrapped)) {
            replay->gap = true;
        }
        if (flags[i] & kFSEventStreamEventFlagHistoryDone) {
            replay->done = true;
        } else if ((flags[i] & (kFSEventStreamEventFlagItemIsFile |
                                kFSEventStreamEventFlagItemIsDir |
                                kFSEventStreamEventFlagItemIsSymlink)) &&
                   !replay->sink(replay->sink_context, paths[i])) {
            replay->gap = true;
            replay->done = true;
        }
    }
}

bool miao_replay_history(uint64_t since, const char *const *roots, size_t count,
                         PathSink sink, void *sink_context) {
    if (since > FSEventsGetCurrentEventId()) return false;
    CFMutableArrayRef paths = CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks);
    for (size_t i = 0; i < count; ++i) {
        CFStringRef path = CFStringCreateWithCString(NULL, roots[i], kCFStringEncodingUTF8);
        if (path) {
            CFArrayAppendValue(paths, path);
            CFRelease(path);
        }
    }
    Replay replay = {sink_context, sink, false, false};
    FSEventStreamContext context = {0, &replay, NULL, NULL, NULL};
    FSEventStreamRef stream = FSEventStreamCreate(NULL, receive_history, &context, paths,
        since, 0.1, kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer);
    CFRelease(paths);
    if (!stream) return false;
    // The run loop and callback state both belong to this worker.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    FSEventStreamScheduleWithRunLoop(stream, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
#pragma clang diagnostic pop
    if (FSEventStreamStart(stream)) {
        double deadline = CFAbsoluteTimeGetCurrent() + 45;
        while (!replay.done && CFAbsoluteTimeGetCurrent() < deadline) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, true);
        }
        FSEventStreamStop(stream);
    }
    FSEventStreamInvalidate(stream);
    FSEventStreamRelease(stream);
    return replay.done && !replay.gap;
}
