#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

#include <stdio.h>
#include <string.h>
#include <unistd.h>

static const NSUInteger BUFFER_BYTES = 64U * 1024U * 1024U;
static const unsigned int MAX_RUNTIME_SECONDS = 5U;

static int expect_command(const char *expected) {
    char command[32];
    if (fgets(command, sizeof(command), stdin) == NULL) {
        return -1;
    }
    command[strcspn(command, "\r\n")] = '\0';
    return strcmp(command, expected) == 0 ? 0 : -1;
}

static int fill_buffer(id<MTLCommandQueue> queue, id<MTLBuffer> buffer) {
    id<MTLCommandBuffer> command = [queue commandBuffer];
    id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
    if (command == nil || blit == nil) {
        return -1;
    }
    [blit fillBuffer:buffer range:NSMakeRange(0, BUFFER_BYTES) value:0xA5];
    [blit endEncoding];
    [command commit];
    [command waitUntilCompleted];
    return command.status == MTLCommandBufferStatusCompleted ? 0 : -1;
}

int main(void) {
    alarm(MAX_RUNTIME_SECONDS);
    setvbuf(stdout, NULL, _IOLBF, 0);
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        id<MTLCommandQueue> queue = [device newCommandQueue];
        if (device == nil || queue == nil) {
            fprintf(stderr, "Metal device or queue unavailable\n");
            return 69;
        }
        printf("READY mode=metal-calibration bytes=%lu\n", (unsigned long)BUFFER_BYTES);
        if (expect_command("allocate") != 0) {
            fprintf(stderr, "expected allocate command\n");
            return 65;
        }

        __strong id<MTLBuffer> buffer = [device newBufferWithLength:BUFFER_BYTES
                                                            options:MTLResourceStorageModeShared];
        if (buffer == nil || fill_buffer(queue, buffer) != 0) {
            fprintf(stderr, "bounded Metal allocation failed\n");
            return 70;
        }
        printf("ALLOCATED\n");
        if (expect_command("release") != 0) {
            fprintf(stderr, "expected release command\n");
            return 65;
        }
        buffer = nil;
        printf("RELEASED\n");
        if (expect_command("exit") != 0) {
            fprintf(stderr, "expected exit command\n");
            return 65;
        }
        printf("EXIT\n");
        return 0;
    }
}
