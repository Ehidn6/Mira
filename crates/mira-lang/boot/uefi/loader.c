/* Mira's small UEFI adapter. Kernel and graphics are compiled from Mira.
 * ABI declarations follow UEFI 2.10; no CRT or third-party runtime is linked.
 */
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef u64 Status;
typedef void *Handle;
#define ERR(n) (0x8000000000000000ULL | (n))
#define PAGE 4096ULL
#define BIGPAGE 0x200000ULL
#define TABLE_PAGES 256
#define MAP_PAGES 64
#define STATE_PAGES (TABLE_PAGES + 1 + MAP_PAGES)
#define MAX_KERNEL_BYTES (64ULL * 1024 * 1024)
#define STACK_PAGES 256
#define PAYLOAD_MAGIC 0x0034364b4152494dULL /* MIRAK64\0 */
#define INFO_MAGIC 0x4f464e494152494dULL /* MIRAINFO */
#define ASSETS_MAGIC 0x313053414152494dULL /* MIRAAS01 */
#define MAX_SOURCE_BYTES (1024ULL * 1024)
#define FONT_BYTES 1024ULL
typedef struct {u32 a;u16 b,c;u8 d[8];} Guid;
typedef struct {u64 signature;u32 revision,size,crc,reserved;} Header;
typedef struct BootServices {
 Header header;
 void *raise_tpl,*restore_tpl;
 Status (*allocate_pages)(u32,u32,u64,u64*);
 Status (*free_pages)(u64,u64);
 Status (*memory_map)(u64*,void*,u64*,u64*,u32*);
 void *allocate_pool;
 Status (*free_pool)(void*);
 void *create_event,*set_timer,*wait_event,*signal_event,*close_event,*check_event;
 void *install_protocol,*reinstall_protocol,*uninstall_protocol;
 Status (*handle_protocol)(Handle,Guid*,void**);
 void *reserved,*register_notify,*locate_handle,*locate_device_path,*install_configuration;
 void *load_image,*start_image,*exit,*unload;
 Status (*exit_boot_services)(Handle,u64);
 void *monotonic_count,*stall;
 Status (*watchdog)(u64,u64,u64,u16*);
 void *connect_controller,*disconnect_controller,*open_protocol,*close_protocol;
 void *open_protocol_information,*protocols_per_handle,*locate_handle_buffer;
 Status (*locate_protocol)(Guid*,void*,void**);
} BootServices;
typedef struct {
 Header header;u16 *vendor;u32 revision;Handle console_in;void *input;
 Handle console_out;void *output;Handle console_err;void *error;
 void *runtime;BootServices *boot;u64 table_count;void *tables;
} SystemTable;
typedef struct {
 u32 revision;Handle parent;SystemTable *system;Handle device;void *path,*reserved;
 u32 options_size;void *options,*image_base;u64 image_size;u32 code_type,data_type;void *unload;
} LoadedImage;
typedef struct File File;
struct File {
 u64 revision;
 Status (*open)(File*,File**,u16*,u64,u64);
 Status (*close)(File*);
 void *remove;
 Status (*read)(File*,u64*,void*);
};
typedef struct {u64 revision;Status (*open_volume)(void*,File**);} FileSystem;
typedef struct {u32 red,green,blue,reserved;} PixelMasks;
typedef struct {u32 version,width,height,format;PixelMasks masks;u32 stride;} ModeInfo;
typedef struct {u32 max_mode,mode;ModeInfo *info;u64 info_size,framebuffer,size;} Mode;
typedef struct Graphics Graphics;
struct Graphics {
 Status (*query)(Graphics*,u32,u64*,ModeInfo**);
 Status (*set)(Graphics*,u32);
 void *blt;Mode *mode;
};
/* All fields are fixed-width u64 so the initial Mira code can read the ABI
 * without depending on C layout, EFI pointers or an EFI graphics protocol.
 */
typedef struct {
 u64 magic,version,size,flags;
 u64 framebuffer,framebuffer_size,width,height,stride,pixel_format;
 u64 red_mask,green_mask,blue_mask,reserved_mask;
 u64 memory_map,memory_map_size,descriptor_size,descriptor_version;
 u64 kernel_base,kernel_size,stack_base,stack_size,page_tables,page_tables_size;
 u64 loader_base,loader_size,state_base,state_size,map_capacity,framebuffer_physical;
 u64 map_format,reserved;
} MiraBootInfo;
/* Optional data only; the v1 boot-info layout remains exactly 256 bytes. */
typedef struct {
 u64 magic,version,size,source_base,source_size,font_base,font_size;
} MiraBootAssets;
_Static_assert(sizeof(MiraBootInfo)==256,"Mira boot-info ABI");
_Static_assert(__builtin_offsetof(MiraBootInfo,reserved)==248,"Mira assets pointer offset");
_Static_assert(sizeof(MiraBootAssets)==56,"Mira boot-assets ABI");
_Static_assert(512+sizeof(MiraBootAssets)<=PAGE,"Mira boot-assets page bounds");
_Static_assert(__builtin_offsetof(BootServices,exit_boot_services)==232,"EFI table layout");
_Static_assert(__builtin_offsetof(SystemTable,boot)==96,"EFI system table layout");
_Static_assert(__builtin_offsetof(LoadedImage,image_base)==64,"EFI loaded-image layout");
static Guid GOP={0x9042a9de,0x23dc,0x4a38,{0x96,0xfb,0x7a,0xde,0xd0,0x80,0x51,0x6a}};
static Guid LOADED={0x5b1b31a1,0x9562,0x11d2,{0x8e,0x3f,0x00,0xa0,0xc9,0x69,0x72,0x3b}};
static Guid FS={0x964e5b22,0x6459,0x11d2,{0x8e,0x39,0x00,0xa0,0xc9,0x69,0x72,0x3b}};
static u16 KERNEL_PATH[]={'\\','k','e','r','n','e','l','.','b','i','n',0};
static u16 SOURCE_PATH[]={'\\','s','o','u','r','c','e','.','m','i','r','a',0};
static u16 FONT_PATH[]={'\\','f','o','n','t','.','b','i','n',0};
static void debug_char(u8 c){__asm__ volatile("outb %0,$0xe9"::"a"(c));}
static void debug(const char *s){while(*s)debug_char((u8)*s++);}
static void hex(u64 n){const char *digits="0123456789abcdef";debug("0x");for(int i=60;i>=0;i-=4)debug_char(digits[(n>>i)&15]);}
__attribute__((noreturn)) static void fail(const char *s){debug("MIRA UEFI ERROR: ");debug(s);debug("\n");for(;;)__asm__ volatile("cli; hlt");}
static void require(Status s,const char *what){if(s & ERR(0))fail(what);}
static void zero(void *p,u64 bytes){u8 *b=p;while(bytes--)*b++=0;}
static u64 allocate(BootServices *b,u64 pages){u64 address=0xffffffffULL;require(b->allocate_pages(1,2,pages,&address),"AllocatePages");zero((void*)address,pages*PAGE);return address;}
/* Optional assets never prevent boot. Read one probe byte after the limit so
 * oversized files cannot silently become valid truncated assets. */
static void read_asset(BootServices *b,File *root,u16 *path,u64 limit,u64 exact,u64 *base,u64 *bytes){
 *base=0;*bytes=0;
 if(!limit || limit>MAX_SOURCE_BYTES || exact>limit)return;
 File *file=0;
 if(root->open(root,&file,path,1,0)&ERR(0))return;
 u64 pages=(limit+PAGE-1)/PAGE,address=0xffffffffULL;
 Status status=b->allocate_pages(1,2,pages,&address);
 if(status&ERR(0)){file->close(file);return;}
 zero((void*)address,pages*PAGE);
 u64 size=limit;
 status=file->read(file,&size,(void*)address);
 int valid=!(status&ERR(0)) && size && size<=limit && (!exact || size==exact);
 if(valid){
  u8 extra=0;u64 probe=1;
  status=file->read(file,&probe,&extra);
  valid=!(status&ERR(0)) && !probe;
 }
 file->close(file);
 if(!valid){zero((void*)address,pages*PAGE);b->free_pages(address,pages);return;}
 *base=address;*bytes=size;
}
static u64 pool_base,pool_used;
static u64 *table(void){if(pool_used==TABLE_PAGES)fail("page table budget");return (u64*)(pool_base+PAGE*pool_used++);}
static u64 *next_table(u64 *parent,u64 index){if(!(parent[index]&1)){parent[index]=(u64)table()|3;}return (u64*)(parent[index]&~0xfffULL);}
static void identity(u64 *pml4,u64 start,u64 size,u64 flags){
 if(!size || start>0x00007fffffffffffULL || size>0x0000800000000000ULL-start)fail("unsupported physical range");
 u64 end=(start+size+BIGPAGE-1)&~(BIGPAGE-1);
 for(u64 p=start&~(BIGPAGE-1);p<end;p+=BIGPAGE){
  u64 *pdpt=next_table(pml4,(p>>39)&511);
  u64 *pd=next_table(pdpt,(p>>30)&511);
  pd[(p>>21)&511]=p|flags;
 }
}
/* Keep descriptor tables in the reserved loader image, not reclaimable firmware RAM. */
__attribute__((used)) static const u64 KERNEL_GDT[]={0,0x00af9a000000ffffULL,0x00cf92000000ffffULL};
typedef struct __attribute__((packed)) {u16 limit;u64 base;} DescriptorTable;
__attribute__((used)) static const DescriptorTable KERNEL_GDTR={sizeof(KERNEL_GDT)-1,(u64)KERNEL_GDT};
__attribute__((used)) static const DescriptorTable EMPTY_IDTR={0,0};
/* Microsoft x64 EFI ABI -> Mira's private stack ABI. No firmware calls beyond
 * this point. The loader, stack, payload and boot-info are mapped in the new CR3.
 */
__attribute__((naked,noreturn)) static void handoff(void *entry,MiraBootInfo *info,u64 stack_top,u64 stack_bottom,u64 cr3){
 __asm__ volatile(
  "mov 40(%rsp),%r10\n"
  "cli\nmov %rcx,%rax\nmov %rdx,%r13\nmov %r9,%r15\n"
  "lgdt KERNEL_GDTR(%rip)\npushq $8\nlea 1f(%rip),%r11\npush %r11\nlretq\n1:\n"
  "mov $0x10,%r11w\nmov %r11w,%ds\nmov %r11w,%es\nmov %r11w,%ss\n"
  "xor %r11d,%r11d\nmov %r11w,%fs\nmov %r11w,%gs\nlidt EMPTY_IDTR(%rip)\n"
  "mov %cr4,%r11\nand $-129,%r11\nmov %r11,%cr4\nmov %r10,%cr3\nmov %r8,%rsp\nand $-16,%rsp\nxor %rbp,%rbp\ncld\njmp *%rax\n");
}
Status efi_main(Handle image,SystemTable *system){
 debug("MIRA UEFI: loader\n");
 BootServices *b=system->boot;
 b->watchdog(0,0,0,0);
 Graphics *g=0;require(b->locate_protocol(&GOP,0,(void**)&g),"GOP missing");
 u32 chosen=g->mode->mode;int found=0,available=0;u64 best_pixels=0;
 for(u32 n=0;n<g->mode->max_mode;n++){
  ModeInfo *info=0;u64 size=0;
  Status status=g->query(g,n,&size,&info);
  if(!(status&ERR(0))){
   int acceptable=info->format<=1 && info->width && info->height && info->stride>=info->width;
   if(acceptable && !available){chosen=n;available=1;}
   /* Prefer a usable text workspace, bounded to avoid enormous/tiny-text modes.
    * Firmware modes remain the source of truth; retain a direct-mode fallback. */
   u64 pixels=(u64)info->width*info->height;
   if(acceptable && info->width>=1024 && info->height>=720 && info->width<=1600 && info->height<=900 && pixels>best_pixels){chosen=n;found=1;best_pixels=pixels;}
   if(!found && acceptable && n==g->mode->mode){chosen=n;}
   b->free_pool(info);
  }
 }
 if(!available)fail("GOP has no direct RGBX/BGRX32 mode");
 require(g->set(g,chosen),"GOP SetMode");
 ModeInfo m=*g->mode->info;
 if(m.format>1 || !m.width || !m.height || m.stride<m.width || !g->mode->framebuffer)fail("GOP needs direct RGBX/BGRX32 framebuffer");
 if((u64)m.stride > (~0ULL)/((u64)m.height*4) || (u64)m.stride*4*m.height>g->mode->size || g->mode->size>0x7fffffffffffffffULL)fail("framebuffer bounds");
 LoadedImage *loaded=0;require(b->handle_protocol(image,&LOADED,(void**)&loaded),"LoadedImage");
 FileSystem *fs=0;require(b->handle_protocol(loaded->device,&FS,(void**)&fs),"boot filesystem");
 File *root=0,*file=0;require(fs->open_volume(fs,&root),"OpenVolume");
 require(root->open(root,&file,KERNEL_PATH,1,0),"kernel.bin missing");
 u64 header[4]={0},size=sizeof(header);
 require(file->read(file,&size,header),"Read kernel.bin header");
 if(size!=sizeof(header) || header[0]!=PAYLOAD_MAGIC || header[1]!=0x0000002000000001ULL || !header[2] || header[2]>MAX_KERNEL_BYTES || header[3]!=32)fail("invalid kernel.bin header");
 u64 kernel_pages=(header[2]+sizeof(header)+PAGE-1)/PAGE;
 u64 kernel=allocate(b,kernel_pages);
 for(u64 n=0;n<4;n++)((u64*)kernel)[n]=header[n];
 size=header[2];
 require(file->read(file,&size,(void*)(kernel+sizeof(header))),"Read kernel.bin body");
 if(size!=header[2])fail("truncated kernel.bin");
 u8 extra=0;size=1;
 require(file->read(file,&size,&extra),"Read kernel.bin end");
 if(size)fail("unexpected kernel.bin trailing bytes");
 file->close(file);
 u64 source=0,source_size=0,font=0,font_size=0;
 read_asset(b,root,SOURCE_PATH,MAX_SOURCE_BYTES,0,&source,&source_size);
 read_asset(b,root,FONT_PATH,FONT_BYTES,FONT_BYTES,&font,&font_size);
 root->close(root);
 u64 stack=allocate(b,STACK_PAGES),state=allocate(b,STATE_PAGES);
 pool_base=state;pool_used=0;
 u64 *pml4=table();
 identity(pml4,0,0x100000000ULL,0x83);
 identity(pml4,kernel,kernel_pages*PAGE,0x83);
 identity(pml4,stack,STACK_PAGES*PAGE,0x83);
 identity(pml4,state,STATE_PAGES*PAGE,0x83);
 if(source_size)identity(pml4,source,MAX_SOURCE_BYTES,0x83);
 if(font_size)identity(pml4,font,PAGE,0x83);
 identity(pml4,(u64)loaded->image_base,loaded->image_size,0x83);
 /* Slot 3 is UC, slot 0 WB in OVMF's PAT. Reject unexpected firmware PAT
  * rather than silently creating a write-back device-memory mapping. */
 u32 pat_low,pat_high;
 __asm__ volatile("rdmsr":"=a"(pat_low),"=d"(pat_high):"c"(0x277));
 if((pat_low&255)!=6 || ((pat_low>>24)&255)!=0)fail("unsupported firmware PAT");
 identity(pml4,g->mode->framebuffer,g->mode->size,0x9b);
 MiraBootInfo *info=(MiraBootInfo*)(state+TABLE_PAGES*PAGE);
 info->magic=INFO_MAGIC;info->version=1;info->size=sizeof(*info);info->flags=1;
 info->framebuffer=g->mode->framebuffer;info->framebuffer_physical=g->mode->framebuffer;
 info->framebuffer_size=g->mode->size;info->width=m.width;info->height=m.height;info->stride=(u64)m.stride*4;
 info->pixel_format=m.format;
 info->red_mask=m.format==0?0xff:0xff0000;info->green_mask=0xff00;info->blue_mask=m.format==0?0xff0000:0xff;info->reserved_mask=0xff000000;
 info->kernel_base=kernel;info->kernel_size=kernel_pages*PAGE;
 info->stack_base=stack;info->stack_size=STACK_PAGES*PAGE;
 info->page_tables=state;info->page_tables_size=TABLE_PAGES*PAGE;
 info->loader_base=(u64)loaded->image_base;info->loader_size=loaded->image_size;
 info->state_base=state;info->state_size=STATE_PAGES*PAGE;
 info->memory_map=(u64)info+PAGE;info->map_capacity=MAP_PAGES*PAGE;info->map_format=1;
 if(source_size || font_size){
  MiraBootAssets *assets=(MiraBootAssets*)((u64)info+512);
  assets->magic=ASSETS_MAGIC;assets->version=1;assets->size=sizeof(*assets);
  assets->source_base=source;assets->source_size=source_size;
  assets->font_base=font;assets->font_size=font_size;
  info->reserved=(u64)assets;info->flags|=1ULL<<4;
 }
 debug("MIRA UEFI: framebuffer=");hex(info->framebuffer);debug(" width=");hex(info->width);debug(" height=");hex(info->height);debug(" stride=");hex(info->stride);debug(" format=");hex(info->pixel_format);debug("\nMIRA UEFI: bootinfo=");hex((u64)info);debug(" cr3=");hex((u64)pml4);debug("\n");
 for(u32 attempt=0;attempt<8;attempt++){
  u64 map_size=info->map_capacity,key=0,descriptor_size=0;u32 version=0;
  require(b->memory_map(&map_size,(void*)info->memory_map,&key,&descriptor_size,&version),"GetMemoryMap");
  if(!descriptor_size || map_size%descriptor_size)fail("memory map descriptor size");
  info->memory_map_size=map_size;info->descriptor_size=descriptor_size;info->descriptor_version=version;
  Status status=b->exit_boot_services(image,key);
  if(!status){__asm__ volatile("cli");info->flags|=2;debug("MIRA UEFI: ExitBootServices OK\n");handoff((void*)(kernel+32),info,stack+STACK_PAGES*PAGE,stack,(u64)pml4);}
  if(status!=ERR(2))fail("ExitBootServices");
 }
 fail("ExitBootServices retry budget");
}
