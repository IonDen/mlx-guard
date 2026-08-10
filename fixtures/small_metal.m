#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static const unsigned long MAX_RUNTIME_MS = 5000UL;
static const NSUInteger BUFFER_BYTES = 4096U;

static int parse_runtime(const char *value, unsigned long *result) {
    errno = 0;
    char *end = NULL;
    unsigned long parsed = strtoul(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed == 0 || parsed > MAX_RUNTIME_MS) {
        return -1;
    }
    *result = parsed;
    return 0;
}

static uint64_t monotonic_nanoseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000000000ULL + (uint64_t)now.tv_nsec;
}

static int submit_small_command(
    id<MTLCommandQueue> queue,
    id<MTLBuffer> buffer,
    uint8_t value
) {
    id<MTLCommandBuffer> command = [queue commandBuffer];
    id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
    if (command == nil || blit == nil) {
        return -1;
    }
    [blit fillBuffer:buffer range:NSMakeRange(0, BUFFER_BYTES) value:value];
    [blit endEncoding];
    [command commit];
    [command waitUntilCompleted];
    return command.status == MTLCommandBufferStatusCompleted ? 0 : -1;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        unsigned long runtime_ms = 0;
        if (argc != 2 || parse_runtime(argv[1], &runtime_ms) != 0) {
            fprintf(stderr, "usage: small_metal RUNTIME_MS (1..5000)\n");
            return 64;
        }

        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        id<MTLCommandQueue> queue = [device newCommandQueue];
        id<MTLBuffer> buffer = [device newBufferWithLength:BUFFER_BYTES
                                                   options:MTLResourceStorageModeShared];
        if (device == nil || queue == nil || buffer == nil) {
            fprintf(stderr, "Metal device, queue, or bounded buffer unavailable\n");
            return 69;
        }
        if (submit_small_command(queue, buffer, 0) != 0) {
            fprintf(stderr, "initial bounded Metal command failed\n");
            return 70;
        }

        setvbuf(stdout, NULL, _IOLBF, 0);
        printf("WORKER_READY kind=metal\n");
        const uint64_t deadline =
            monotonic_nanoseconds() + (uint64_t)runtime_ms * 1000000ULL;
        uint8_t value = 1;
        while (monotonic_nanoseconds() < deadline) {
            @autoreleasepool {
                if (submit_small_command(queue, buffer, value) != 0) {
                    fprintf(stderr, "bounded Metal command failed\n");
                    return 70;
                }
            }
            value += 1;
        }
        return 0;
    }
}
