#import <Metal/Metal.h>
int main(int c,char**v){@autoreleasepool{
 id<MTLDevice> d=MTLCreateSystemDefaultDevice();
 NSError*e=nil; id<MTLLibrary> l=[d newLibraryWithURL:[NSURL fileURLWithPath:@(v[1])] error:&e];
 NSLog(@"lib=%@ err=%@ names=%@ type=%ld",l,e,l.functionNames,(long)l.type);
 for(NSString*n in l.functionNames){id<MTLFunction> f=[l newFunctionWithName:n]; NSLog(@"fn %@ type=%ld",n,(long)f.functionType);}
}}
