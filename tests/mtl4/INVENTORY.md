# Metal 4 / macOS 26-27 protocol surface inventory

Generated from the clang AST of `#import <Metal/Metal.h>` in the macOS 27.0 SDK minus the same dump of the macOS 15.5 SDK
(so: every protocol member added in macOS 26 or 27). Columns: member, kind, availability (blank = inherited from the protocol),
@optional, whether a method with that full selector (or property getter) is defined in plugin/*.m at the base commit 02aeaf8 and in this branch
(a text match over all classes, so a hit can be a different class with the same selector: use the runtime lister to confirm), and owner.

Owners: mtl4core = MTL4 queue/allocator/command buffer/encoders/argument tables; mtl4compiler = MTL4Compiler, pipelines, archive, serializer; mtl4extras = everything else.


## MTL4Archive

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-label` | method |  |  | yes | yes | mtl4compiler |
| `-newBinaryFunctionWithDescriptor:error:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:dynamicLinkingDescriptor:error:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:error:` | method |  |  | yes | yes | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:dynamicLinkingDescriptor:error:` | method |  |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:error:` | method |  |  | yes | yes | mtl4compiler |
| `-setLabel:` | method |  |  | yes | yes | mtl4compiler |
| `@label` | property |  |  | yes | yes | mtl4compiler |

## MTL4ArgumentTable

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-device` | method |  |  | yes | yes | mtl4core |
| `-label` | method |  |  | yes | yes | mtl4core |
| `-setAddress:atIndex:` | method |  |  | no | no | mtl4core |
| `-setAddress:attributeStride:atIndex:` | method |  |  | no | no | mtl4core |
| `-setResource:atBufferIndex:` | method |  |  | no | no | mtl4core |
| `-setSamplerState:atIndex:` | method |  |  | yes | yes | mtl4core |
| `-setTexture:atIndex:` | method |  |  | yes | yes | mtl4core |
| `@device` | property |  |  | yes | yes | mtl4core |
| `@label` | property |  |  | yes | yes | mtl4core |

## MTL4BinaryFunction

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-functionType` | method |  |  | yes | yes | mtl4compiler |
| `-name` | method |  |  | yes | yes | mtl4compiler |
| `@functionType` | property |  |  | yes | yes | mtl4compiler |
| `@name` | property |  |  | yes | yes | mtl4compiler |

## MTL4CommandAllocator

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-allocatedSize` | method |  |  | yes | yes | mtl4core |
| `-device` | method |  |  | yes | yes | mtl4core |
| `-label` | method |  |  | yes | yes | mtl4core |
| `-reset` | method |  |  | yes | yes | mtl4core |
| `@device` | property |  |  | yes | yes | mtl4core |
| `@label` | property |  |  | yes | yes | mtl4core |

## MTL4CommandBuffer

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-beginCommandBufferWithAllocator:` | method |  |  | no | no | mtl4core |
| `-beginCommandBufferWithAllocator:options:` | method |  |  | no | no | mtl4core |
| `-computeCommandEncoder` | method |  |  | yes | yes | mtl4core |
| `-device` | method |  |  | yes | yes | mtl4core |
| `-endCommandBuffer` | method |  |  | no | no | mtl4core |
| `-label` | method |  |  | yes | yes | mtl4core |
| `-machineLearningCommandEncoder` | method |  |  | no | no | mtl4core |
| `-popDebugGroup` | method |  |  | yes | yes | mtl4core |
| `-pushDebugGroup:` | method |  |  | yes | yes | mtl4core |
| `-renderCommandEncoderWithDescriptor:` | method |  |  | yes | yes | mtl4core |
| `-renderCommandEncoderWithDescriptor:options:` | method |  |  | no | no | mtl4core |
| `-resolveCounterHeap:withRange:intoBuffer:waitFence:updateFence:` | method | 26.0 |  | no | no | mtl4core |
| `-setLabel:` | method |  |  | yes | yes | mtl4core |
| `-useResidencySet:` | method |  |  | yes | yes | mtl4core |
| `-useResidencySets:count:` | method |  |  | yes | yes | mtl4core |
| `-writeTimestampIntoHeap:atIndex:` | method | 26.0 |  | no | no | mtl4core |
| `@device` | property |  |  | yes | yes | mtl4core |
| `@label` | property |  |  | yes | yes | mtl4core |

## MTL4CommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-barrierAfterEncoderStages:beforeEncoderStages:visibilityOptions:` | method |  |  | no | no | mtl4core |
| `-barrierAfterQueueStages:beforeStages:visibilityOptions:` | method |  |  | no | no | mtl4core |
| `-barrierAfterStages:beforeQueueStages:visibilityOptions:` | method |  |  | no | no | mtl4core |
| `-commandBuffer` | method |  |  | yes | yes | mtl4core |
| `-endEncoding` | method |  |  | yes | yes | mtl4core |
| `-insertDebugSignpost:` | method |  |  | yes | yes | mtl4core |
| `-label` | method |  |  | yes | yes | mtl4core |
| `-popDebugGroup` | method |  |  | yes | yes | mtl4core |
| `-pushDebugGroup:` | method |  |  | yes | yes | mtl4core |
| `-setLabel:` | method |  |  | yes | yes | mtl4core |
| `-updateFence:afterEncoderStages:` | method |  |  | no | no | mtl4core |
| `-waitForFence:beforeEncoderStages:` | method |  |  | no | no | mtl4core |
| `@commandBuffer` | property |  |  | yes | yes | mtl4core |
| `@label` | property |  |  | yes | yes | mtl4core |

## MTL4CommandQueue

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-addResidencySet:` | method |  |  | yes | yes | mtl4core |
| `-addResidencySets:count:` | method |  |  | yes | yes | mtl4core |
| `-commit:count:` | method |  |  | no | no | mtl4core |
| `-commit:count:options:` | method |  |  | no | no | mtl4core |
| `-copyBufferMappingsFromBuffer:toBuffer:operations:count:` | method |  |  | no | no | mtl4core |
| `-copyTextureMappingsFromTexture:toTexture:operations:count:` | method |  |  | no | no | mtl4core |
| `-device` | method |  |  | yes | yes | mtl4core |
| `-label` | method |  |  | yes | yes | mtl4core |
| `-removeResidencySet:` | method |  |  | yes | yes | mtl4core |
| `-removeResidencySets:count:` | method |  |  | yes | yes | mtl4core |
| `-signalDrawable:` | method |  |  | no | no | mtl4core |
| `-signalEvent:value:` | method |  |  | no | no | mtl4core |
| `-updateBufferMappings:heap:operations:count:` | method |  |  | no | no | mtl4core |
| `-updateTextureMappings:heap:operations:count:` | method |  |  | no | no | mtl4core |
| `-waitForDrawable:` | method |  |  | no | no | mtl4core |
| `-waitForEvent:value:` | method |  |  | no | no | mtl4core |
| `@device` | property |  |  | yes | yes | mtl4core |
| `@label` | property |  |  | yes | yes | mtl4core |

## MTL4CommitFeedback

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-GPUEndTime` | method |  |  | yes | yes | mtl4core |
| `-GPUStartTime` | method |  |  | yes | yes | mtl4core |
| `-error` | method |  |  | yes | yes | mtl4core |
| `@GPUEndTime` | property |  |  | yes | yes | mtl4core |
| `@GPUStartTime` | property |  |  | yes | yes | mtl4core |
| `@error` | property |  |  | yes | yes | mtl4core |

## MTL4Compiler

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-device` | method |  |  | yes | yes | mtl4compiler |
| `-label` | method |  |  | yes | yes | mtl4compiler |
| `-newBinaryFunctionWithDescriptor:compilerTaskOptions:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newBinaryFunctionWithDescriptor:compilerTaskOptions:error:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:compilerTaskOptions:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:compilerTaskOptions:error:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:dynamicLinkingDescriptor:compilerTaskOptions:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithDescriptor:dynamicLinkingDescriptor:compilerTaskOptions:error:` | method |  |  | no | no | mtl4compiler |
| `-newDynamicLibrary:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newDynamicLibrary:error:` | method |  |  | yes | yes | mtl4compiler |
| `-newDynamicLibraryWithURL:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newDynamicLibraryWithURL:error:` | method |  |  | yes | yes | mtl4compiler |
| `-newLibraryWithDescriptor:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newLibraryWithDescriptor:error:` | method |  |  | no | no | mtl4compiler |
| `-newMachineLearningPipelineStateWithDescriptor:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newMachineLearningPipelineStateWithDescriptor:error:` | method |  |  | no | no | mtl4compiler |
| `-newRenderPipelineStateBySpecializationWithDescriptor:pipeline:completionHandler:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newRenderPipelineStateBySpecializationWithDescriptor:pipeline:error:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:compilerTaskOptions:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:compilerTaskOptions:error:` | method |  |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:dynamicLinkingDescriptor:compilerTaskOptions:completionHandler:` | method |  |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithDescriptor:dynamicLinkingDescriptor:compilerTaskOptions:error:` | method |  |  | no | no | mtl4compiler |
| `-pipelineDataSetSerializer` | method |  |  | no | no | mtl4compiler |
| `@device` | property |  |  | yes | yes | mtl4compiler |
| `@label` | property |  |  | yes | yes | mtl4compiler |
| `@pipelineDataSetSerializer` | property |  |  | no | no | mtl4compiler |

## MTL4CompilerTask

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-compiler` | method |  |  | yes | yes | mtl4compiler |
| `-status` | method |  |  | yes | yes | mtl4compiler |
| `-waitUntilCompleted` | method |  |  | yes | yes | mtl4compiler |
| `@compiler` | property |  |  | yes | yes | mtl4compiler |
| `@status` | property |  |  | yes | yes | mtl4compiler |

## MTL4ComputeCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-buildAccelerationStructure:descriptor:scratchBuffer:` | method |  |  | yes | yes | mtl4core |
| `-copyAccelerationStructure:toAccelerationStructure:` | method |  |  | yes | yes | mtl4core |
| `-copyAndCompactAccelerationStructure:toAccelerationStructure:` | method |  |  | yes | yes | mtl4core |
| `-copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:` | method |  |  | yes | yes | mtl4core |
| `-copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:options:` | method |  |  | yes | yes | mtl4core |
| `-copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:` | method |  |  | yes | yes | mtl4core |
| `-copyFromTensor:sourceOrigin:sourceDimensions:sourcePlane:toTensor:destinationOrigin:destinationDimensions:destinationPlane:` | method | 27.0 |  | no | yes | mtl4core |
| `-copyFromTensor:sourceOrigin:sourceDimensions:toTensor:destinationOrigin:destinationDimensions:` | method |  |  | no | yes | mtl4core |
| `-copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:` | method |  |  | yes | yes | mtl4core |
| `-copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:options:` | method |  |  | yes | yes | mtl4core |
| `-copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:` | method |  |  | yes | yes | mtl4core |
| `-copyFromTexture:sourceSlice:sourceLevel:toTexture:destinationSlice:destinationLevel:sliceCount:levelCount:` | method |  |  | yes | yes | mtl4core |
| `-copyFromTexture:toTexture:` | method |  |  | yes | yes | mtl4core |
| `-copyIndirectCommandBuffer:sourceRange:destination:destinationIndex:` | method |  |  | yes | yes | mtl4core |
| `-dispatchThreadgroups:threadsPerThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-dispatchThreadgroupsWithIndirectBuffer:threadsPerThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-dispatchThreads:threadsPerThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-dispatchThreadsWithIndirectBuffer:` | method |  |  | no | no | mtl4core |
| `-executeCommandsInBuffer:indirectBuffer:` | method |  |  | no | no | mtl4core |
| `-executeCommandsInBuffer:withRange:` | method |  |  | yes | yes | mtl4core |
| `-fillBuffer:range:value:` | method |  |  | yes | yes | mtl4core |
| `-generateMipmapsForTexture:` | method |  |  | yes | yes | mtl4core |
| `-optimizeContentsForCPUAccess:` | method |  |  | yes | yes | mtl4core |
| `-optimizeContentsForCPUAccess:slice:level:` | method |  |  | yes | yes | mtl4core |
| `-optimizeContentsForGPUAccess:` | method |  |  | yes | yes | mtl4core |
| `-optimizeContentsForGPUAccess:slice:level:` | method |  |  | yes | yes | mtl4core |
| `-optimizeIndirectCommandBuffer:withRange:` | method |  |  | yes | yes | mtl4core |
| `-refitAccelerationStructure:descriptor:destination:scratchBuffer:` | method |  |  | yes | yes | mtl4core |
| `-refitAccelerationStructure:descriptor:destination:scratchBuffer:options:` | method |  |  | yes | yes | mtl4core |
| `-resetCommandsInBuffer:withRange:` | method |  |  | yes | yes | mtl4core |
| `-setArgumentTable:` | method |  |  | no | no | mtl4core |
| `-setComputePipelineState:` | method |  |  | yes | yes | mtl4core |
| `-setImageblockWidth:height:` | method |  |  | yes | yes | mtl4core |
| `-setThreadgroupMemoryLength:atIndex:` | method |  |  | yes | yes | mtl4core |
| `-stages` | method |  |  | no | no | mtl4core |
| `-writeCompactedAccelerationStructureSize:toBuffer:` | method |  |  | yes | yes | mtl4core |
| `-writeTimestampWithGranularity:intoHeap:atIndex:` | method |  |  | no | no | mtl4core |

## MTL4CounterHeap

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-count` | method |  |  | yes | yes | mtl4extras |
| `-invalidateCounterRange:` | method |  |  | no | yes | mtl4extras |
| `-label` | method |  |  | yes | yes | mtl4extras |
| `-resolveCounterRange:` | method |  |  | yes | yes | mtl4extras |
| `-setLabel:` | method |  |  | yes | yes | mtl4extras |
| `-type` | method |  |  | yes | yes | mtl4extras |
| `@count` | property |  |  | yes | yes | mtl4extras |
| `@label` | property |  |  | yes | yes | mtl4extras |
| `@type` | property |  |  | yes | yes | mtl4extras |

## MTL4MachineLearningCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-dispatchNetworkWithIntermediatesHeap:` | method |  |  | no | no | mtl4extras |
| `-setArgumentTable:` | method |  |  | no | no | mtl4extras |
| `-setPipelineState:` | method |  |  | no | no | mtl4extras |

## MTL4MachineLearningPipelineState

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-device` | method |  |  | yes | yes | mtl4extras |
| `-intermediatesHeapSize` | method |  |  | no | no | mtl4extras |
| `-label` | method |  |  | yes | yes | mtl4extras |
| `-reflection` | method |  |  | no | no | mtl4extras |
| `@device` | property |  |  | yes | yes | mtl4extras |
| `@intermediatesHeapSize` | property |  |  | no | no | mtl4extras |
| `@label` | property |  |  | yes | yes | mtl4extras |
| `@reflection` | property |  |  | no | no | mtl4extras |

## MTL4PipelineDataSetSerializer

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-serializeAsArchiveAndFlushToURL:error:` | method |  |  | no | no | mtl4compiler |
| `-serializeAsPipelinesScriptWithError:` | method |  |  | no | no | mtl4compiler |

## MTL4RenderCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-dispatchThreadsPerTile:` | method |  |  | yes | yes | mtl4core |
| `-drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferLength:` | method |  |  | no | no | mtl4core |
| `-drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferLength:instanceCount:` | method |  |  | no | no | mtl4core |
| `-drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferLength:instanceCount:baseVertex:baseInstance:` | method |  |  | no | no | mtl4core |
| `-drawIndexedPrimitives:indexType:indexBuffer:indexBufferLength:indirectBuffer:` | method |  |  | no | no | mtl4core |
| `-drawMeshThreadgroups:threadsPerObjectThreadgroup:threadsPerMeshThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-drawMeshThreadgroupsWithIndirectBuffer:threadsPerObjectThreadgroup:threadsPerMeshThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-drawMeshThreads:threadsPerObjectThreadgroup:threadsPerMeshThreadgroup:` | method |  |  | yes | yes | mtl4core |
| `-drawPrimitives:indirectBuffer:` | method |  |  | yes | yes | mtl4core |
| `-drawPrimitives:vertexStart:vertexCount:` | method |  |  | yes | yes | mtl4core |
| `-drawPrimitives:vertexStart:vertexCount:instanceCount:` | method |  |  | yes | yes | mtl4core |
| `-drawPrimitives:vertexStart:vertexCount:instanceCount:baseInstance:` | method |  |  | yes | yes | mtl4core |
| `-executeCommandsInBuffer:indirectBuffer:` | method |  |  | no | no | mtl4core |
| `-executeCommandsInBuffer:withRange:` | method |  |  | yes | yes | mtl4core |
| `-setArgumentTable:atStages:` | method |  |  | no | no | mtl4core |
| `-setBlendColorRed:green:blue:alpha:` | method |  |  | yes | yes | mtl4core |
| `-setColorAttachmentMap:` | method |  |  | no | yes | mtl4core |
| `-setColorStoreAction:atIndex:` | method |  |  | yes | yes | mtl4core |
| `-setCullMode:` | method |  |  | yes | yes | mtl4core |
| `-setDepthBias:slopeScale:clamp:` | method |  |  | yes | yes | mtl4core |
| `-setDepthClipMode:` | method |  |  | yes | yes | mtl4core |
| `-setDepthStencilState:` | method |  |  | yes | yes | mtl4core |
| `-setDepthStoreAction:` | method |  |  | yes | yes | mtl4core |
| `-setDepthTestMinBound:maxBound:` | method |  |  | yes | yes | mtl4core |
| `-setFrontFacingWinding:` | method |  |  | yes | yes | mtl4core |
| `-setObjectThreadgroupMemoryLength:atIndex:` | method |  |  | yes | yes | mtl4core |
| `-setRenderPipelineState:` | method |  |  | yes | yes | mtl4core |
| `-setScissorRect:` | method |  |  | yes | yes | mtl4core |
| `-setScissorRects:count:` | method |  |  | yes | yes | mtl4core |
| `-setStencilFrontReferenceValue:backReferenceValue:` | method |  |  | yes | yes | mtl4core |
| `-setStencilReferenceValue:` | method |  |  | yes | yes | mtl4core |
| `-setStencilStoreAction:` | method |  |  | yes | yes | mtl4core |
| `-setThreadgroupMemoryLength:offset:atIndex:` | method |  |  | yes | yes | mtl4core |
| `-setTriangleFillMode:` | method |  |  | yes | yes | mtl4core |
| `-setVertexAmplificationCount:viewMappings:` | method |  |  | no | no | mtl4core |
| `-setViewport:` | method |  |  | yes | yes | mtl4core |
| `-setViewports:count:` | method |  |  | yes | yes | mtl4core |
| `-setVisibilityResultMode:offset:` | method |  |  | yes | yes | mtl4core |
| `-tileHeight` | method |  |  | yes | yes | mtl4core |
| `-tileWidth` | method |  |  | yes | yes | mtl4core |
| `-writeTimestampWithGranularity:afterStage:intoHeap:atIndex:` | method |  |  | no | no | mtl4core |
| `@tileHeight` | property |  |  | yes | yes | mtl4core |
| `@tileWidth` | property |  |  | yes | yes | mtl4core |

## MTLArgumentEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-setDepthStencilState:atIndex:` | method | 26.0 |  | no | yes | mtl4extras |
| `-setDepthStencilStates:withRange:` | method | 26.0 |  | no | yes | mtl4extras |

## MTLBlitCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-copyFromTensor:sourceOrigin:sourceDimensions:sourcePlane:toTensor:destinationOrigin:destinationDimensions:destinationPlane:` | method | 27.0 |  | no | yes | mtl4extras |
| `-copyFromTensor:sourceOrigin:sourceDimensions:toTensor:destinationOrigin:destinationDimensions:` | method | 26.0 |  | no | yes | mtl4extras |

## MTLBuffer

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-newTensorWithDescriptor:offset:error:` | method | 26.0 |  | no | yes | mtl4extras |
| `-sparseBufferTier` | method | 26.0 |  | no | yes | mtl4extras |
| `@sparseBufferTier` | property | 26.0 |  | no | yes | mtl4extras |

## MTLCaptureScope

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-mtl4CommandQueue` | method |  |  | no | no | mtl4extras |
| `@mtl4CommandQueue` | property |  |  | no | no | mtl4extras |

## MTLCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-barrierAfterQueueStages:beforeStages:` | method | 26.0 |  | no | yes | mtl4extras |

## MTLComputePipelineState

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-functionHandleWithBinaryFunction:` | method | 26.0 |  | no | no | mtl4compiler |
| `-functionHandleWithName:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newComputePipelineStateWithBinaryFunctions:error:` | method | 26.0 |  | no | no | mtl4compiler |
| `-reflection` | method | 26.0 |  | no | no | mtl4compiler |
| `-requiredThreadsPerThreadgroup` | method | 26.0 |  | no | no | mtl4compiler |
| `@reflection` | property | 26.0 |  | no | no | mtl4compiler |
| `@requiredThreadsPerThreadgroup` | property | 26.0 |  | no | no | mtl4compiler |

## MTLDepthStencilState

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-gpuResourceID` | method | 26.0 |  | yes | yes | mtl4extras |
| `@gpuResourceID` | property | 26.0 |  | yes | yes | mtl4extras |

## MTLDevice

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-functionHandleWithBinaryFunction:` | method | 26.0 |  | no | no | mtl4compiler |
| `-functionHandleWithFunction:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-newArchiveWithURL:error:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newArgumentTableWithDescriptor:error:` | method | 26.0 |  | no | no | mtl4core |
| `-newBufferWithLength:options:placementSparsePageSize:` | method | 26.0 |  | no | yes | mtl4extras |
| `-newCommandAllocator` | method | 26.0 |  | no | no | mtl4core |
| `-newCommandAllocatorWithDescriptor:error:` | method | 26.0 |  | no | no | mtl4core |
| `-newCommandBuffer` | method | 26.0 |  | no | no | mtl4core |
| `-newCompilerWithDescriptor:error:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newCounterHeapWithDescriptor:error:` | method | 26.0 |  | no | yes | mtl4extras |
| `-newMTL4CommandQueue` | method | 26.0 |  | no | no | mtl4core |
| `-newMTL4CommandQueueWithDescriptor:error:` | method | 26.0 |  | no | no | mtl4core |
| `-newPipelineDataSetSerializerWithDescriptor:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newTensorWithDescriptor:attachments:error:` | method | 27.0 |  | no | yes | mtl4extras |
| `-newTensorWithDescriptor:error:` | method | 26.0 |  | no | yes | mtl4extras |
| `-newTextureViewPoolWithDescriptor:error:` | method | 26.0 |  | no | yes | mtl4extras |
| `-queryTimestampFrequency` | method | 26.0 |  | no | yes | mtl4extras |
| `-sizeOfCounterHeapEntry:` | method | 26.0 |  | no | yes | mtl4extras |
| `-supportsPlacementSparse` | method | 26.4 |  | no | yes | mtl4extras |
| `-tensorSizeAndAlignWithDescriptor:` | method | 26.0 |  | no | yes | mtl4extras |
| `@supportsPlacementSparse` | property | 26.4 |  | no | yes | mtl4extras |

## MTLFunctionHandle

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-gpuResourceID` | method | 26.0 |  | yes | yes | mtl4extras |
| `@gpuResourceID` | property | 26.0 |  | yes | yes | mtl4extras |

## MTLIndirectRenderCommand

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-setCullMode:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-setDepthBias:slopeScale:clamp:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-setDepthClipMode:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-setDepthStencilState:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-setFrontFacingWinding:` | method | 26.0 |  | yes | yes | mtl4extras |
| `-setTriangleFillMode:` | method | 26.0 |  | yes | yes | mtl4extras |

## MTLLibrary

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-reflectionForFunctionWithName:` | method | 26.0 |  | no | yes | mtl4extras |

## MTLRenderCommandEncoder

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-setColorAttachmentMap:` | method | 26.0 |  | no | yes | mtl4extras |
| `-setDepthTestMinBound:maxBound:` | method | 26.0 |  | yes | yes | mtl4extras |

## MTLRenderPipelineState

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-functionHandleWithBinaryFunction:stage:` | method | 26.0 |  | no | no | mtl4compiler |
| `-functionHandleWithName:stage:` | method | 26.0 |  | no | no | mtl4compiler |
| `-newRenderPipelineDescriptorForSpecialization` | method | 26.0 |  | no | no | mtl4compiler |
| `-newRenderPipelineStateWithBinaryFunctions:error:` | method | 26.0 |  | no | no | mtl4compiler |
| `-reflection` | method | 26.0 |  | no | no | mtl4compiler |
| `-requiredThreadsPerMeshThreadgroup` | method | 26.0 |  | no | no | mtl4compiler |
| `-requiredThreadsPerObjectThreadgroup` | method | 26.0 |  | no | no | mtl4compiler |
| `-requiredThreadsPerTileThreadgroup` | method | 26.0 |  | no | no | mtl4compiler |
| `@reflection` | property | 26.0 |  | no | no | mtl4compiler |
| `@requiredThreadsPerMeshThreadgroup` | property | 26.0 |  | no | no | mtl4compiler |
| `@requiredThreadsPerObjectThreadgroup` | property | 26.0 |  | no | no | mtl4compiler |
| `@requiredThreadsPerTileThreadgroup` | property | 26.0 |  | no | no | mtl4compiler |

## MTLResourceViewPool

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-baseResourceID` | method |  |  | no | yes | mtl4extras |
| `-copyResourceViewsFromPool:sourceRange:destinationIndex:` | method |  |  | no | yes | mtl4extras |
| `-device` | method |  |  | yes | yes | mtl4extras |
| `-label` | method |  |  | yes | yes | mtl4extras |
| `-resourceViewCount` | method |  |  | no | yes | mtl4extras |
| `@baseResourceID` | property |  |  | no | yes | mtl4extras |
| `@device` | property |  |  | yes | yes | mtl4extras |
| `@label` | property |  |  | yes | yes | mtl4extras |
| `@resourceViewCount` | property |  |  | no | yes | mtl4extras |

## MTLTensor

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-auxiliaryPlanes` | method | 27.0 |  | no | yes | mtl4extras |
| `-buffer` | method |  |  | yes | yes | mtl4extras |
| `-bufferOffset` | method |  |  | yes | yes | mtl4extras |
| `-dataType` | method |  |  | yes | yes | mtl4extras |
| `-dimensions` | method |  |  | no | yes | mtl4extras |
| `-getBytes:strides:fromSliceOrigin:sliceDimensions:` | method |  |  | no | yes | mtl4extras |
| `-getBytes:strides:fromSliceOrigin:sliceDimensions:plane:` | method | 27.0 |  | no | yes | mtl4extras |
| `-gpuResourceID` | method |  |  | yes | yes | mtl4extras |
| `-replaceSliceOrigin:sliceDimensions:plane:withBytes:strides:` | method | 27.0 |  | no | yes | mtl4extras |
| `-replaceSliceOrigin:sliceDimensions:withBytes:strides:` | method |  |  | no | yes | mtl4extras |
| `-strides` | method |  |  | no | yes | mtl4extras |
| `-usage` | method |  |  | yes | yes | mtl4extras |
| `@auxiliaryPlanes` | property | 27.0 |  | no | yes | mtl4extras |
| `@buffer` | property |  |  | yes | yes | mtl4extras |
| `@bufferOffset` | property |  |  | yes | yes | mtl4extras |
| `@dataType` | property |  |  | yes | yes | mtl4extras |
| `@dimensions` | property |  |  | no | yes | mtl4extras |
| `@gpuResourceID` | property |  |  | yes | yes | mtl4extras |
| `@strides` | property |  |  | no | yes | mtl4extras |
| `@usage` | property |  |  | yes | yes | mtl4extras |

## MTLTensorAuxiliaryPlane

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-blockFactors` | method |  |  | no | yes | mtl4extras |
| `-buffer` | method |  |  | yes | yes | mtl4extras |
| `-bufferOffset` | method |  |  | yes | yes | mtl4extras |
| `-dataType` | method |  |  | yes | yes | mtl4extras |
| `-planeType` | method |  |  | no | yes | mtl4extras |
| `@blockFactors` | property |  |  | no | yes | mtl4extras |
| `@buffer` | property |  |  | yes | yes | mtl4extras |
| `@bufferOffset` | property |  |  | yes | yes | mtl4extras |
| `@dataType` | property |  |  | yes | yes | mtl4extras |
| `@planeType` | property |  |  | no | yes | mtl4extras |

## MTLTensorBinding

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-auxiliaryPlanes` | method | 27.0 |  | no | yes | mtl4extras |
| `-dimensions` | method |  |  | no | yes | mtl4extras |
| `-indexType` | method |  |  | no | yes | mtl4extras |
| `-tensorDataType` | method |  |  | no | yes | mtl4extras |
| `@auxiliaryPlanes` | property | 27.0 |  | no | yes | mtl4extras |
| `@dimensions` | property |  |  | no | yes | mtl4extras |
| `@indexType` | property |  |  | no | yes | mtl4extras |
| `@tensorDataType` | property |  |  | no | yes | mtl4extras |

## MTLTexture

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-newTextureViewWithDescriptor:` | method | 26.0 |  | no | yes | mtl4extras |
| `-sparseTextureTier` | method | 26.0 |  | no | yes | mtl4extras |
| `@sparseTextureTier` | property | 26.0 |  | no | yes | mtl4extras |

## MTLTextureViewPool

| member | kind | macos | optional | in plugin at base | in this branch | owner |
|---|---|---|---|---|---|---|
| `-setTextureView:atIndex:` | method |  |  | no | yes | mtl4extras |
| `-setTextureView:descriptor:atIndex:` | method |  |  | no | yes | mtl4extras |
| `-setTextureViewFromBuffer:descriptor:offset:bytesPerRow:atIndex:` | method |  |  | no | yes | mtl4extras |

## Totals

| owner | in plugin at base | not at base | present in this branch | still missing in this branch |
|---|---|---|---|---|
| mtl4core | 110 | 47 | 113 | 44 |
| mtl4compiler | 20 | 48 | 20 | 48 |
| mtl4extras | 44 | 63 | 98 | 9 |

## Tools and notes

- `list_protocol_selectors --load <NVMTLDriver> --protocol MTLDevice --class NVMTLDevice` prints the selectors the plugin class does not
  answer; on an Apple GPU driver class it prints nothing.
- `mtl4_extras_oracle` (built by `build_extras.sh`) runs the counter heap, tensor, view pool, sparse buffer and texture, color map and
  reflection arms. Run the arm64 build on an Apple GPU first, then the x86_64 build on the NVIDIA device.
- `plugin_logic_harness.m` compiles the plugin files unchanged against stand-ins and runs the same arms, plus direct checks of the sparse
  mapping tables, over them on an Apple GPU. It does not exercise Vulkan.
- Not in the plugin on purpose: `MTL4MachineLearning*` (the ml agent), `MTLCaptureScope.mtl4CommandQueue` (Apple's capture manager makes the
  scope; the oracle checks it round-trips a Metal 4 queue). `MTLTensorBinding` is built from a "Tensor" entry of the translator reflection
  JSON, whose contract is in plugin/NVMTLTensor.h; no translator emits it yet.
- Sparse textures (2D, 2D array, 3D colour textures; sparse heaps, placement sparse textures with the 64 KB page, the Metal 3 resource state
  encoder, Metal 4 queue operations) are built in plugin/NVMTLSparseTex.m over the Vulkan sparse image calls in nvmtl_vk.c. A tile is the GPU's
  64 KB binding block; 16 KB and 256 KB placement pages and cube, depth or multisample sparse textures are refused.
