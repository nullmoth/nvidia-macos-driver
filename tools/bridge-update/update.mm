// A constrained, reversible supplement for the published 3.0.1 Wine provider.
// No kernel extensions, renderer, graphics DLLs or Minecraft files are changed.
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#include <CommonCrypto/CommonDigest.h>
#include <mach-o/loader.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <cerrno>
#include <cstdio>
#include <stdexcept>
#include <string>

static NSString *const updateID=@"3.0.1-nvk-hybrid-1";
static NSFileManager *fm;
static void need(bool ok,NSString *message) {
   if(!ok)throw std::runtime_error(message.UTF8String);
}
static NSString *join(NSString *root,NSString *relative) {
   need(relative.length && !relative.isAbsolutePath,@"Invalid relative path");
   for(NSString *part in relative.pathComponents)
      need(![part isEqual:@".."] && ![part isEqual:@"."] && part.length,@"Unsafe relative path");
   return [root stringByAppendingPathComponent:relative];
}
static bool exists(NSString *path) { struct stat s{};return !lstat(path.fileSystemRepresentation,&s); }
static void directory(NSString *path,bool create) {
   need(path.isAbsolutePath,@"An absolute directory is required");
   NSString *current=@"/";
   for(NSString *part in path.pathComponents) {
      if([part isEqual:@"/"])continue;
      current=[current stringByAppendingPathComponent:part];struct stat s{};
      if(lstat(current.fileSystemRepresentation,&s)) {
         need(errno==ENOENT && create,@"Required directory is missing or cannot be inspected");
         need(!mkdir(current.fileSystemRepresentation,0755),@"Cannot create update directory");
         need(!lstat(current.fileSystemRepresentation,&s),@"Cannot inspect update directory");
      }
      need(S_ISDIR(s.st_mode),@"Directory contains a symbolic link or non-directory");
#ifndef NVK_BRIDGE_UPDATE_TEST
      need(s.st_uid==0 && !(s.st_mode&0022),@"System directory is not protected from modification");
#endif
   }
}
static NSData *readFile(NSString *root,NSString *relative) {
   NSString *path=join(root,relative);directory(path.stringByDeletingLastPathComponent,false);
   struct stat s{};need(!lstat(path.fileSystemRepresentation,&s) && S_ISREG(s.st_mode),
      [@"Missing regular file: " stringByAppendingString:relative]);
   NSData *data=[NSData dataWithContentsOfFile:path options:NSDataReadingUncached error:nil];
   need(data!=nil,[@"Cannot read: " stringByAppendingString:relative]);return data;
}
// Package directories need not be root-owned. Still reject traversal and links.
static NSData *readPackageFile(NSString *root,NSString *relative) {
   NSString *path=join(root,relative),*current=root;
   for(NSString *part in relative.pathComponents) {
      current=[current stringByAppendingPathComponent:part];struct stat s{};
      need(!lstat(current.fileSystemRepresentation,&s) && !S_ISLNK(s.st_mode),@"Package file is missing or linked");
   }
   struct stat s{};need(!lstat(path.fileSystemRepresentation,&s) && S_ISREG(s.st_mode),@"Package file is not regular");
   NSData *data=[NSData dataWithContentsOfFile:path options:NSDataReadingUncached error:nil];
   need(data!=nil,@"Cannot read package file");return data;
}
static NSString *sha(NSData *data) {
   CC_SHA256_CTX ctx;CC_SHA256_Init(&ctx);
   const unsigned char *p=(const unsigned char *)data.bytes;NSUInteger remaining=data.length;
   while(remaining) { CC_LONG n=(CC_LONG)MIN(remaining,(NSUInteger)UINT32_MAX);CC_SHA256_Update(&ctx,p,n);p+=n;remaining-=n; }
   unsigned char bytes[CC_SHA256_DIGEST_LENGTH];CC_SHA256_Final(bytes,&ctx);
   char result[CC_SHA256_DIGEST_LENGTH*2+1];
   for(unsigned j=0;j<sizeof(bytes);j++)snprintf(result+j*2,3,"%02x",bytes[j]);
   return @(result);
}
static NSDictionary *json(NSData *data) {
   id value=[NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
   need([value isKindOfClass:NSDictionary.class],@"Invalid JSON record");return value;
}
static NSData *encoded(NSDictionary *value) {
   NSData *data=[NSJSONSerialization dataWithJSONObject:value options:NSJSONWritingPrettyPrinted|NSJSONWritingSortedKeys error:nil];
   need(data!=nil,@"Cannot encode update record");return data;
}
static void writeFile(NSString *path,NSData *data,mode_t mode,uid_t owner,gid_t group) {
   NSString *parent=path.stringByDeletingLastPathComponent;directory(parent,false);
   std::string temporary=std::string(parent.fileSystemRepresentation)+"/.nvk-bridge-XXXXXX";
   int fd=mkstemp(temporary.data());need(fd>=0,@"Cannot create atomic replacement");
   try {
      need(!fchmod(fd,mode),@"Cannot set replacement permissions");
      if(geteuid()==0)need(!fchown(fd,owner,group),@"Cannot preserve file ownership");
      const char *p=(const char *)data.bytes;NSUInteger remaining=data.length;
      while(remaining) { ssize_t n=write(fd,p,remaining);if(n<0 && errno==EINTR)continue;
         need(n>0,@"Cannot write replacement");p+=n;remaining-=n; }
      need(!fsync(fd),@"Cannot flush replacement");
      need(!rename(temporary.c_str(),path.fileSystemRepresentation),@"Cannot replace file atomically");
      close(fd);fd=-1;int dir=open(parent.fileSystemRepresentation,O_RDONLY|O_DIRECTORY);
      need(dir>=0,@"Cannot open replacement directory");int result=fsync(dir);close(dir);
      need(!result,@"Cannot flush replacement directory");
   } catch(...) { if(fd>=0)close(fd);unlink(temporary.c_str());throw; }
}
static void writeState(NSString *backup,NSMutableDictionary *state,NSString *phase) {
   state[@"phase"]=phase;writeFile(join(backup,@"state.json"),encoded(state),0600,geteuid(),getegid());
}
static void signedBinary(NSString *path,NSData *data) {
   need(data.length>=sizeof(mach_header_64),@"Truncated executable");
   mach_header_64 header{};memcpy(&header,data.bytes,sizeof(header));
   need(header.magic==MH_MAGIC_64 && header.cputype==CPU_TYPE_X86_64,@"Expected an Intel 64-bit binary");
   SecStaticCodeRef code=nullptr;
   OSStatus result=SecStaticCodeCreateWithPath((__bridge CFURLRef)[NSURL fileURLWithPath:path],kSecCSDefaultFlags,&code);
   if(result==errSecSuccess)result=SecStaticCodeCheckValidity(code,kSecCSStrictValidate|kSecCSCheckAllArchitectures,nullptr);
   if(code)CFRelease(code);need(result==errSecSuccess,@"Executable signature verification failed");
}
static void idleProvider(NSString *provider) {
   NSTask *task=[NSTask new];task.executableURL=[NSURL fileURLWithPath:@"/usr/sbin/lsof"];
   task.arguments=@[@"-n",@"-P",@"-t",@"--",join(provider,@"lib/libnvk_wine.dylib"),join(provider,@"lib/libnvk_renderer.dylib")];
   NSPipe *output=[NSPipe pipe];task.standardOutput=output;task.standardError=output;
   NSError *error=nil;need([task launchAndReturnError:&error],@"Cannot check running Wine processes");
   NSData *data=[output.fileHandleForReading readDataToEndOfFile];[task waitUntilExit];
   NSString *message=[[NSString alloc]initWithData:data encoding:NSUTF8StringEncoding];
   need(task.terminationStatus==1 && data.length==0,
      [@"Close all Wine/CrossOver/YAAGL games and launchers before updating. Open files or inspection error: " stringByAppendingString:message?:@""]);
}
static NSArray<NSString *> *changedPaths() {
   return @[@"lib/libnvk_wine.dylib",@"FRONTEND-BUILD.json",@"BUILD.json",@"nvk-wine-runtime.json",@"SHA256SUMS"];
}
static void validateAlias(NSString *provider,NSDictionary *spec) {
   need([spec[@"symlinks"] isEqual:@{@"lib/libMoltenVK.dylib":@"libnvk_wine.dylib"}],@"Unexpected provider aliases");
   for(NSString *relative in spec[@"symlinks"])
      need([[fm destinationOfSymbolicLinkAtPath:join(provider,relative) error:nil] isEqual:spec[@"symlinks"][relative]],@"Provider alias was modified");
}
static NSDictionary *currentFiles(NSString *provider,NSDictionary *spec) {
   NSMutableDictionary *current=[NSMutableDictionary dictionary];validateAlias(provider,spec);
   for(NSString *relative in spec[@"before"])
      current[relative]=sha(readFile(provider,relative));
   return current;
}
static bool matches(NSDictionary *current,NSDictionary *expected) { return [current isEqual:expected]; }
static void allowedMixed(NSDictionary *current,NSDictionary *spec) {
   for(NSString *relative in spec[@"before"])
      need([current[relative] isEqual:spec[@"before"][relative]] || [current[relative] isEqual:spec[@"after"][relative]],
         [@"Provider differs from this update; preserving: " stringByAppendingString:relative]);
}
static void restore(NSString *provider,NSString *backup,NSDictionary *spec,NSMutableDictionary *state) {
   NSMutableDictionary *originals=[NSMutableDictionary dictionary];
   for(NSString *relative in changedPaths()) {
      NSData *data=readFile(backup,[@"originals/" stringByAppendingString:relative]);
      need([sha(data) isEqual:spec[@"before"][relative]],@"Rollback backup verification failed");originals[relative]=data;
      need([state[@"attributes"][relative] isKindOfClass:NSDictionary.class],@"Missing rollback file attributes");
   }
   writeState(backup,state,@"restoring");
   for(NSString *relative in changedPaths()) {
      NSDictionary *attr=state[@"attributes"][relative];
      writeFile(join(provider,relative),originals[relative],[attr[@"mode"] unsignedShortValue],
         [attr[@"uid"] unsignedIntValue],[attr[@"gid"] unsignedIntValue]);
   }
   need(matches(currentFiles(provider,spec),spec[@"before"]),@"Rollback verification failed");
   writeState(backup,state,@"restored");
}

int main(int argc,char **argv) { @autoreleasepool { try {
   fm=NSFileManager.defaultManager;
   need(argc>=4,@"Usage: nvk-bridge-update check|install|rollback --package directory");
   NSString *command=@(argv[1]),*package=nil;int failAfter=-1,crashAfter=-1;
#ifdef NVK_BRIDGE_UPDATE_TEST
   NSString *testRoot=nil;
#endif
   need([@[@"check",@"install",@"rollback"] containsObject:command],@"Unknown command");
   for(int j=2;j<argc;j++) {
      NSString *option=@(argv[j]);need(j+1<argc,@"Option needs a value");NSString *value=@(argv[++j]);
      if([option isEqual:@"--package"])package=value;
#ifdef NVK_BRIDGE_UPDATE_TEST
      else if([option isEqual:@"--root"])testRoot=value;
      else if([option isEqual:@"--fail-after"])failAfter=value.intValue;
      else if([option isEqual:@"--crash-after"])crashAfter=value.intValue;
#endif
      else need(false,@"Unknown option");
   }
   need(package.isAbsolutePath,@"An absolute package directory is required");package=package.stringByResolvingSymlinksInPath;
   need([NSProcessInfo.processInfo isOperatingSystemAtLeastVersion:{15,5,0}],@"Requires macOS 15.5 or newer");
   NSData *specData=readPackageFile(package,@"UPDATE.json");NSDictionary *spec=json(specData);
   need([spec[@"schema_version"] intValue]==1 && [spec[@"update_id"] isEqual:updateID],@"Unsupported update identity");
   need([spec[@"base_release"] isEqual:@"v3.0.1-nvk"],@"Wrong base driver version");
   NSDictionary *before=spec[@"before"],*after=spec[@"after"];
   need([before isKindOfClass:NSDictionary.class] && before.count>=changedPaths().count &&
      [after isKindOfClass:NSDictionary.class] && [NSSet setWithArray:before.allKeys].count==after.count &&
      [[NSSet setWithArray:before.allKeys] isEqual:[NSSet setWithArray:after.allKeys]],@"Invalid provider file set");
   for(NSString *relative in before) {
      join(@"/",relative);
      need([before[relative] isKindOfClass:NSString.class] && [before[relative] length]==64 &&
         [after[relative] isKindOfClass:NSString.class] && [after[relative] length]==64,@"Invalid provider digest");
      if(![changedPaths() containsObject:relative])need([before[relative] isEqual:after[relative]],@"Update changes an unrelated provider component");
   }
   NSMutableDictionary *payload=[NSMutableDictionary dictionary];
   need([spec[@"package_files"] isKindOfClass:NSDictionary.class],@"Missing package hashes");
   for(NSString *relative in spec[@"package_files"]) {
      NSData *data=readPackageFile(package,relative);
      need([sha(data) isEqual:spec[@"package_files"][relative]],[@"Package checksum failed: " stringByAppendingString:relative]);
      payload[relative]=data;
   }
   for(NSString *relative in changedPaths()) {
      NSString *file=[@"payload/" stringByAppendingString:relative];
      need(before[relative] && payload[file] && [sha(payload[file]) isEqual:after[relative]],@"Missing or invalid replacement");
   }
   signedBinary(join(package,@"bin/nvk-bridge-update"),payload[@"bin/nvk-bridge-update"]);
   signedBinary(join(package,@"payload/lib/libnvk_wine.dylib"),payload[@"payload/lib/libnvk_wine.dylib"]);
   NSString *prefix=@"";
#ifdef NVK_BRIDGE_UPDATE_TEST
   need(testRoot.isAbsolutePath && ![testRoot isEqual:@"/"],@"Test build requires an isolated --root");prefix=testRoot;
#endif
   NSString *provider=[prefix stringByAppendingString:@"/Library/GPUBundles/nvmtl/wine"];
   NSString *backup=[prefix stringByAppendingFormat:@"/Library/Application Support/NVK-Bridge-Updates/%@",updateID];
   directory(provider,false);
   if([command isEqual:@"check"]) {
      NSDictionary *current=currentFiles(provider,spec);
      if(matches(current,after))puts("UPDATED: Wine bridge update is installed.");
      else if(matches(current,before))puts("READY: verified v3.0.1-nvk provider; Wine bridge update can be installed.");
      else need(false,@"Provider is modified or an interrupted update needs install/rollback recovery");
      return 0;
   }
#ifndef NVK_BRIDGE_UPDATE_TEST
   need(geteuid()==0,@"Run install/rollback with sudo");
#endif
   directory(backup,true);
   int lock=open(join(backup,@"lock").fileSystemRepresentation,O_CREAT|O_RDWR|O_NOFOLLOW,0600);
   need(lock>=0 && !flock(lock,LOCK_EX|LOCK_NB),@"Another bridge update is running");
   struct stat lockStat{};need(!fstat(lock,&lockStat) && S_ISREG(lockStat.st_mode) && lockStat.st_uid==geteuid(),@"Unsafe update lock");
   NSDictionary *current=currentFiles(provider,spec);allowedMixed(current,spec);
   NSMutableDictionary *state=nil;
   if(exists(join(backup,@"state.json"))) {
      state=[json(readFile(backup,@"state.json")) mutableCopy];
      need([state[@"spec_sha256"] isEqual:sha(specData)] && [state[@"update_id"] isEqual:updateID],@"Backup belongs to a different updater");
   }
   if([command isEqual:@"rollback"]) {
      need(state!=nil,@"No rollback backup exists");
      if(matches(current,before)) { puts("RESTORED: original provider is already installed.");return 0; }
      need(matches(current,after) || [@[@"prepared",@"restoring"] containsObject:state[@"phase"]],@"Unexpected partially modified provider");
      idleProvider(provider);restore(provider,backup,spec,state);puts("RESTORED: verified original Wine bridge and manifests.");return 0;
   }
   if(matches(current,after)) { puts("UPDATED: already installed; no files or backups changed.");return 0; }
   idleProvider(provider);
   if(!matches(current,before)) {
      need(state && [@[@"prepared",@"restoring"] containsObject:state[@"phase"]],@"Cannot recover provider without its matching journal");
      restore(provider,backup,spec,state);current=currentFiles(provider,spec);
   }
   need(matches(current,before),@"Provider is not the supported v3.0.1-nvk baseline");
   state=[@{@"schema_version":@1,@"update_id":updateID,@"spec_sha256":sha(specData)} mutableCopy];
   NSMutableDictionary *attributes=[NSMutableDictionary dictionary];
   for(NSString *relative in changedPaths()) {
      struct stat s{};need(!lstat(join(provider,relative).fileSystemRepresentation,&s),@"Cannot inspect original file");
      attributes[relative]=@{@"mode":@(s.st_mode&0777),@"uid":@(s.st_uid),@"gid":@(s.st_gid)};
   }
   state[@"attributes"]=attributes;writeState(backup,state,@"backing-up");
   for(NSString *relative in changedPaths()) {
      NSString *saved=join(backup,[@"originals/" stringByAppendingString:relative]);directory(saved.stringByDeletingLastPathComponent,true);
      NSData *original=readFile(provider,relative);need([sha(original) isEqual:before[relative]],@"Provider changed during backup");
      writeFile(saved,original,0600,geteuid(),getegid());
      need([sha(readFile(backup,[@"originals/" stringByAppendingString:relative])) isEqual:before[relative]],@"Backup checksum failed");
   }
   idleProvider(provider);need(matches(currentFiles(provider,spec),before),@"Provider changed before installation");
   writeState(backup,state,@"prepared");
   try {
      int written=0;
      for(NSString *relative in changedPaths()) {
         NSDictionary *attr=attributes[relative];
         writeFile(join(provider,relative),payload[[@"payload/" stringByAppendingString:relative]],
            [attr[@"mode"] unsignedShortValue],[attr[@"uid"] unsignedIntValue],[attr[@"gid"] unsignedIntValue]);
         ++written;
         if(written==crashAfter)_exit(86);
         need(written!=failAfter,@"Injected installation failure");
      }
      need(matches(currentFiles(provider,spec),after),@"Installed provider verification failed");
      writeState(backup,state,@"installed");
   } catch(...) { restore(provider,backup,spec,state);throw; }
   puts("UPDATED: Wine bridge and provider manifests verified. Relaunch your games; no reboot is needed.");
   printf("Rollback: sudo \"%s/bin/nvk-bridge-update\" rollback --package \"%s\"\n",package.fileSystemRepresentation,package.fileSystemRepresentation);
   return 0;
} catch(const std::exception &error) { fprintf(stderr,"NVK bridge updater: %s\n",error.what());return 1; } } }
