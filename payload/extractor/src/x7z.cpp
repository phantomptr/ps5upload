// Every format but RAR, through 7-Zip's archive handlers (payload/third_party/7zip, extract-only,
// single-threaded): 7z (with AES), zip (deflate, deflate64, bzip2, lzma, ppmd, xz, zstd, ZipCrypto,
// WinZip AES), tar, gz, bz2, xz, zst, and numbered split sets (x.7z.001). A tar inside gz/bz2/xz/zst
// (x.tar.gz, x.tgz, ...) is unpacked in one pass: one thread decodes the outer stream into a pipe
// the tar reader takes in order, so nothing is written twice.
//
// What is written, and where, is decided by the sink (xsink.c); this file only maps 7-Zip's items
// and results onto it.

#include "Common/Common.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <unistd.h>

#include "Common/ComTry.h"
#include "Common/MyCom.h"
#include "Common/MyString.h"
#include "Common/UTFConvert.h"
#include "Windows/PropVariant.h"

#include "7zip/Archive/IArchive.h"
#include "7zip/IPassword.h"
#include "7zip/PropID.h"
#include "7zip/Common/RegisterArc.h"

#include "xfree.h"
#include "xjob.h"

#ifdef __PROSPERO__
#include "proc_identity.h"
#endif

// ---- the handler registry (7-Zip's own lives in ArchiveExports.cpp, a DLL's export table) ----

static const CArcInfo *g_Arcs[32];
static unsigned g_NumArcs;

void RegisterArc(const CArcInfo *arcInfo) throw() {
  if (g_NumArcs < sizeof g_Arcs / sizeof g_Arcs[0]) g_Arcs[g_NumArcs++] = arcInfo;
}

static const CArcInfo *FindArc(const char *name) {
  for (unsigned i = 0; i < g_NumArcs; i++)
    if (strcasecmp(g_Arcs[i]->Name, name) == 0) return g_Arcs[i];
  return NULL;
}

// ---- streams ----

// Where the archive is read, while it is freed as it goes (xfree.h): the lowest offset of the set
// (its parts read as one stream) read since the last Reset, and whether a deleted part was asked for.
struct CReadTrack {
  const uint64_t *VStart; // each part's first byte in the set; NULL: nothing is tracked
  uint64_t Lo;
  bool Any;
  bool BadRead;
  void Reset() { Lo = 0; Any = false; }
};
static CReadTrack g_track;

// An archive file (or one volume), read through its descriptor.
class CFdInStream Z7_final:
  public IInStream,
  public IStreamGetSize,
  public CMyUnknownImp
{
  Z7_COM_UNKNOWN_IMP_3(IInStream, ISequentialInStream, IStreamGetSize)
  Z7_IFACE_COM7_IMP(ISequentialInStream)
  Z7_IFACE_COM7_IMP(IInStream)
  Z7_IFACE_COM7_IMP(IStreamGetSize)
  int _fd;
  bool _dropped;
public:
  int Vol; // which part of the archive this is (-1: not one of its parts)
  CFdInStream(): _fd(-1), _dropped(false), Vol(-1) {}
  ~CFdInStream() { if (_fd >= 0) close(_fd); }
  bool Open(const char *path) {
    _fd = open(path, O_RDONLY);
    return _fd >= 0;
  }
  // The part is about to be deleted: its descriptor goes (an open file keeps its space), and a
  // read of it after this is a failure, never a silent short read.
  void Drop() {
    if (_fd >= 0) close(_fd);
    _fd = -1;
    _dropped = true;
  }
};

Z7_COM7F_IMF(CFdInStream::Read(void *data, UInt32 size, UInt32 *processedSize)) {
  if (processedSize) *processedSize = 0;
  if (_fd < 0) {
    if (_dropped) g_track.BadRead = true;
    return E_FAIL;
  }
  off_t at = -1;
  if (g_track.VStart && Vol >= 0 && size) at = lseek(_fd, 0, SEEK_CUR);
  for (;;) {
    ssize_t n = read(_fd, data, size);
    if (n < 0 && errno == EINTR) continue;
    if (n < 0) return HRESULT_FROM_WIN32(errno);
    if (processedSize) *processedSize = (UInt32)n;
    if (n > 0 && at >= 0) {
      uint64_t v = g_track.VStart[Vol] + (uint64_t)at;
      if (!g_track.Any || v < g_track.Lo) g_track.Lo = v;
      g_track.Any = true;
    }
    return S_OK;
  }
}

Z7_COM7F_IMF(CFdInStream::Seek(Int64 offset, UInt32 seekOrigin, UInt64 *newPosition)) {
  int whence = seekOrigin == STREAM_SEEK_SET ? SEEK_SET : seekOrigin == STREAM_SEEK_CUR ? SEEK_CUR : SEEK_END;
  off_t r = lseek(_fd, (off_t)offset, whence);
  if (r < 0) return HRESULT_FROM_WIN32(errno == EINVAL ? EINVAL : errno);
  if (newPosition) *newPosition = (UInt64)r;
  return S_OK;
}

Z7_COM7F_IMF(CFdInStream::GetSize(UInt64 *size)) {
  struct stat st;
  if (fstat(_fd, &st) != 0) return HRESULT_FROM_WIN32(errno);
  *size = (UInt64)st.st_size;
  return S_OK;
}

// One entry's bytes, handed to the sink.
Z7_CLASS_IMP_COM_1(CSinkOutStream, ISequentialOutStream)
public:
  xjob_t *J;
  xentry_t *E;
  bool Failed;
  CSinkOutStream(): J(NULL), E(NULL), Failed(false) {}
};

Z7_COM7F_IMF(CSinkOutStream::Write(const void *data, UInt32 size, UInt32 *processedSize)) {
  if (processedSize) *processedSize = 0;
  if (Failed) return E_ABORT;
  if (size && xs_write(J, E, data, size) != 0) {
    Failed = true;
    return E_ABORT;
  }
  if (processedSize) *processedSize = size;
  return S_OK;
}

// Bytes decoded only to be counted: the check before freeing (every unit read through once), and
// an entry the sink does not write while freeing (it is still decoded, so the archive is read
// exactly as the check read it).
Z7_CLASS_IMP_COM_1(CDiscardOutStream, ISequentialOutStream)
public:
  xjob_t *J;
  CDiscardOutStream(): J(NULL) {}
};

Z7_COM7F_IMF(CDiscardOutStream::Write(const void *, UInt32 size, UInt32 *processedSize)) {
  if (processedSize) *processedSize = size;
  J->bytes_done += size;
  if (J->progress) J->progress(J, 0);
  return xj_cancelled(J) ? E_ABORT : S_OK;
}

// A bounded pipe between the thread decoding the outer stream of a .tar.gz (and the like) and the
// tar reader. Closing either end wakes the other.
class CPipe {
  pthread_mutex_t _m;
  pthread_cond_t _c;
  Byte *_buf;
  size_t _cap, _head, _len;
  bool _writeClosed, _readClosed;
public:
  CPipe(size_t cap): _buf(new Byte[cap]), _cap(cap), _head(0), _len(0), _writeClosed(false), _readClosed(false) {
    pthread_mutex_init(&_m, NULL);
    pthread_cond_init(&_c, NULL);
  }
  ~CPipe() {
    pthread_cond_destroy(&_c);
    pthread_mutex_destroy(&_m);
    delete[] _buf;
  }
  // false: the reader is gone.
  bool Write(const Byte *p, size_t n) {
    pthread_mutex_lock(&_m);
    while (n) {
      while (_len == _cap && !_readClosed) pthread_cond_wait(&_c, &_m);
      if (_readClosed) {
        pthread_mutex_unlock(&_m);
        return false;
      }
      size_t tail = (_head + _len) % _cap;
      size_t room = _cap - _len, run = _cap - tail;
      size_t take = n < room ? n : room;
      if (take > run) take = run;
      memcpy(_buf + tail, p, take);
      _len += take;
      p += take;
      n -= take;
      pthread_cond_broadcast(&_c);
    }
    pthread_mutex_unlock(&_m);
    return true;
  }
  // 0 at the end of the stream.
  size_t Read(Byte *p, size_t n) {
    pthread_mutex_lock(&_m);
    while (_len == 0 && !_writeClosed && !_readClosed) pthread_cond_wait(&_c, &_m);
    size_t run = _cap - _head, take = n < _len ? n : _len;
    if (take > run) take = run;
    memcpy(p, _buf + _head, take);
    _head = (_head + take) % _cap;
    _len -= take;
    pthread_cond_broadcast(&_c);
    pthread_mutex_unlock(&_m);
    return take;
  }
  void CloseWrite() {
    pthread_mutex_lock(&_m);
    _writeClosed = true;
    pthread_cond_broadcast(&_c);
    pthread_mutex_unlock(&_m);
  }
  void CloseRead() {
    pthread_mutex_lock(&_m);
    _readClosed = true;
    pthread_cond_broadcast(&_c);
    pthread_mutex_unlock(&_m);
  }
};

Z7_CLASS_IMP_COM_1(CPipeOut, ISequentialOutStream)
public:
  CPipe *P;
};

Z7_COM7F_IMF(CPipeOut::Write(const void *data, UInt32 size, UInt32 *processedSize)) {
  if (processedSize) *processedSize = 0;
  if (!P->Write((const Byte *)data, size)) return E_ABORT;
  if (processedSize) *processedSize = size;
  return S_OK;
}

Z7_CLASS_IMP_COM_1(CPipeIn, ISequentialInStream)
public:
  CPipe *P;
};

Z7_COM7F_IMF(CPipeIn::Read(void *data, UInt32 size, UInt32 *processedSize)) {
  size_t n = P->Read((Byte *)data, size);
  if (processedSize) *processedSize = (UInt32)n;
  return S_OK;
}

// ---- open ----

static void Utf8ToU(const char *s, UString &u) {
  AString a(s);
  ConvertUTF8ToUnicode(a, u);
}

static void UToUtf8(const UString &u, AString &a) { ConvertUnicodeToUTF8(u, a); }

// What the open and extract callbacks share: the password, and whether one was asked for.
struct CPasswordState {
  xjob_t *J;
  bool Asked;
  CPasswordState(xjob_t *j): J(j), Asked(false) {}
  HRESULT Get(BSTR *password) {
    Asked = true;
    if (!J->has_password) return E_ABORT;
    UString u;
    Utf8ToU(J->password, u);
    return StringToBstr(u, password);
  }
};

class COpenCallback Z7_final:
  public IArchiveOpenCallback,
  public IArchiveOpenVolumeCallback,
  public ICryptoGetTextPassword,
  public CMyUnknownImp
{
  Z7_IFACES_IMP_UNK_3(IArchiveOpenCallback, IArchiveOpenVolumeCallback, ICryptoGetTextPassword)
public:
  xjob_t *J;
  CPasswordState *Pw;
  AString Dir;      // the archive's folder, with a trailing '/'
  UString FirstName; // the first volume's own name
  bool VolumeMissing;
  AString MissingName;
  // Every file of the archive opened, in the order opened: the first, then the volumes a split set
  // asked for. Held for the length of the job (each can be dropped before it is deleted).
  CRecordVector<CFdInStream *> Vols;
  CObjectVector<AString> VolPaths;
  COpenCallback(): J(NULL), Pw(NULL), VolumeMissing(false) {}
  ~COpenCallback() {
    for (unsigned i = 0; i < Vols.Size(); i++) static_cast<IInStream *>(Vols[i])->Release();
  }
  void AddVol(CFdInStream *s, const AString &path) {
    static_cast<IInStream *>(s)->AddRef();
    s->Vol = (int)Vols.Size();
    Vols.Add(s);
    VolPaths.Add(path);
  }
};

Z7_COM7F_IMF(COpenCallback::SetTotal(const UInt64 *, const UInt64 *)) { return S_OK; }

Z7_COM7F_IMF(COpenCallback::SetCompleted(const UInt64 *, const UInt64 *)) {
  return xj_cancelled(J) ? E_ABORT : S_OK;
}

Z7_COM7F_IMF(COpenCallback::GetProperty(PROPID propID, PROPVARIANT *value)) {
  NWindows::NCOM::CPropVariant prop;
  if (propID == kpidName) prop = FirstName;
  prop.Detach(value);
  return S_OK;
}

Z7_COM7F_IMF(COpenCallback::GetStream(const wchar_t *name, IInStream **inStream)) {
  *inStream = NULL;
  UString u(name);
  AString a;
  UToUtf8(u, a);
  // A sibling volume only: never a path.
  if (a.IsEmpty() || a.Find('/') >= 0 || a == "." || a == "..") return S_FALSE;
  AString path = Dir;
  path += a;
  CFdInStream *spec = new CFdInStream;
  CMyComPtr<IInStream> s = spec;
  if (!spec->Open(path)) {
    if (!VolumeMissing) {
      VolumeMissing = true;
      MissingName = a;
    }
    return S_FALSE;
  }
  AddVol(spec, path);
  *inStream = s.Detach();
  return S_OK;
}

Z7_COM7F_IMF(COpenCallback::CryptoGetTextPassword(BSTR *password)) { return Pw->Get(password); }

// Opens `stream` as `arc`'s format: S_OK, S_FALSE (not that format) or an error.
static HRESULT OpenAs(const CArcInfo *info, IInStream *stream, IArchiveOpenCallback *cb, CMyComPtr<IInArchive> &out) {
  CMyComPtr<IInArchive> arc = info->CreateInArchive();
  if (!arc) return E_OUTOFMEMORY;
  RINOK(stream->Seek(0, STREAM_SEEK_SET, NULL))
  const UInt64 maxStart = 0;
  HRESULT r = arc->Open(stream, &maxStart, cb);
  if (r == S_OK) out = arc;
  else arc->Close();
  return r;
}

static bool ExtMatches(const char *list, const char *ext) {
  // `list` is 7-Zip's space-separated extension list ("gz gzip tgz tpz apk").
  size_t n = strlen(ext);
  if (!n) return false;
  for (const char *p = list; p && *p;) {
    const char *sp = strchr(p, ' ');
    size_t len = sp ? (size_t)(sp - p) : strlen(p);
    if (len == n && strncasecmp(p, ext, n) == 0) return true;
    p = sp ? sp + 1 : NULL;
  }
  return false;
}

static bool SigMatches(const CArcInfo *info, const Byte *head, size_t have) {
  if (!info->SignatureSize) return false;
  if (info->IsMultiSignature()) {
    const Byte *s = info->Signature;
    for (unsigned left = info->SignatureSize; left > 0;) {
      unsigned len = *s++;
      if (len + 1 > left) break;
      if (info->SignatureOffset + len <= have && memcmp(head + info->SignatureOffset, s, len) == 0) return true;
      s += len;
      left -= len + 1;
    }
    return false;
  }
  return info->SignatureOffset + info->SignatureSize <= have &&
         memcmp(head + info->SignatureOffset, info->Signature, info->SignatureSize) == 0;
}

// The formats this extractor opens (RAR is xrar.c's), in the order a name's extension picks them.
static const char *const kFormats[] = {"7z", "zip", "tar", "gzip", "bzip2", "xz", "zstd", "lzma", "Split"};

// Opens `stream` (named `name`): the format the extension names first, then any whose signature is
// at the start. S_OK with arc set, S_FALSE when no format takes it, or an error (E_ABORT: a password
// was needed and none was given, or a stop).
static HRESULT OpenAny(const char *name, IInStream *stream, IArchiveOpenCallback *cb, CMyComPtr<IInArchive> &arc,
                       const CArcInfo **used) {
  const char *dot = strrchr(name, '.');
  const char *ext = dot ? dot + 1 : "";
  Byte head[64];
  UInt32 have = 0;
  RINOK(stream->Seek(0, STREAM_SEEK_SET, NULL))
  RINOK(stream->Read(head, sizeof head, &have))
  const CArcInfo *tried[16];
  unsigned ntried = 0;
  for (int pass = 0; pass < 2; pass++) {
    for (unsigned k = 0; k < sizeof kFormats / sizeof kFormats[0]; k++) {
      const CArcInfo *info = FindArc(kFormats[k]);
      if (!info) continue;
      bool byExt = ExtMatches(info->Ext, ext);
      if (pass == 0 && !byExt) continue;
      if (pass == 1) {
        bool seen = false;
        for (unsigned t = 0; t < ntried; t++) seen |= tried[t] == info;
        if (seen || !SigMatches(info, head, have)) continue;
      }
      if (ntried < 16) tried[ntried++] = info;
      HRESULT r = OpenAs(info, stream, cb, arc);
      if (r == S_OK) {
        *used = info;
        return S_OK;
      }
      if (r != S_FALSE) return r;
    }
  }
  return S_FALSE;
}

// ---- extract ----

static bool GetBool(IInArchive *arc, UInt32 i, PROPID id) {
  NWindows::NCOM::CPropVariant p;
  return arc->GetProperty(i, id, &p) == S_OK && p.vt == VT_BOOL && p.boolVal != VARIANT_FALSE;
}

static bool GetU64(IInArchive *arc, UInt32 i, PROPID id, UInt64 &v) {
  NWindows::NCOM::CPropVariant p;
  if (arc->GetProperty(i, id, &p) != S_OK) return false;
  if (p.vt == VT_UI8) v = p.uhVal.QuadPart;
  else if (p.vt == VT_UI4) v = p.ulVal;
  else return false;
  return true;
}

static bool GetStr(IInArchive *arc, UInt32 i, PROPID id, AString &out) {
  NWindows::NCOM::CPropVariant p;
  if (arc->GetProperty(i, id, &p) != S_OK || p.vt != VT_BSTR || !p.bstrVal) return false;
  UString u(p.bstrVal);
  UToUtf8(u, out);
  return true;
}

// The item's kind: a folder, a plain file, or something that is never created (a link, a device,
// an alternate stream, an "anti" item that deletes).
static int ItemKind(IInArchive *arc, UInt32 i) {
  if (GetBool(arc, i, kpidIsDir)) return XE_DIR;
  if (GetBool(arc, i, kpidIsAltStream) || GetBool(arc, i, kpidIsAnti)) return XE_OTHER;
  AString link;
  if (GetStr(arc, i, kpidSymLink, link) && !link.IsEmpty()) return XE_LINK;
  if (GetStr(arc, i, kpidHardLink, link) && !link.IsEmpty()) return XE_LINK;
  UInt64 mode;
  if (GetU64(arc, i, kpidPosixAttrib, mode)) {
    unsigned t = (unsigned)mode & 0170000u;
    if (t == 0040000u) return XE_DIR;
    if (t == 0120000u) return XE_LINK;
    if (t != 0 && t != 0100000u) return XE_OTHER;
  } else if (GetU64(arc, i, kpidAttrib, mode) && (mode & 0x8000u)) {
    // FILE_ATTRIBUTE_UNIX_EXTENSION: the POSIX mode in the high 16 bits.
    unsigned t = (unsigned)(mode >> 16) & 0170000u;
    if (t == 0120000u) return XE_LINK;
    if (t != 0 && t != 0100000u && t != 0040000u) return XE_OTHER;
  }
  return XE_FILE;
}

static int64_t ItemMTime(IInArchive *arc, UInt32 i) {
  NWindows::NCOM::CPropVariant p;
  if (arc->GetProperty(i, kpidMTime, &p) != S_OK || p.vt != VT_FILETIME) return -1;
  UInt64 ft = ((UInt64)p.filetime.dwHighDateTime << 32) | p.filetime.dwLowDateTime;
  const UInt64 kUnixEpoch = 116444736000000000ull; // 1970-01-01 in 100 ns units since 1601
  if (ft < kUnixEpoch) return -1;
  return (int64_t)((ft - kUnixEpoch) / 10000000ull);
}

// The name of an item that has none (a .gz without a stored name): the archive's own name without
// its last extension.
static void DefaultItemName(const char *archive, AString &out) {
  const char *slash = strrchr(archive, '/');
  AString base(slash ? slash + 1 : archive);
  int dot = base.ReverseFind('.');
  if (dot > 0) base.DeleteFrom((unsigned)dot);
  if (base.IsEmpty()) base = "extracted";
  out = base;
}

class CExtractCallback Z7_final:
  public IArchiveExtractCallback,
  public ICryptoGetTextPassword,
  public CMyUnknownImp
{
  Z7_IFACES_IMP_UNK_2(IArchiveExtractCallback, ICryptoGetTextPassword)
  Z7_IFACE_COM7_IMP(IProgress)
public:
  xjob_t *J;
  CPasswordState *Pw;
  IInArchive *Arc;
  const char *Archive;
  bool Encrypted; // the current item is encrypted
  bool AnyEncrypted; // some item is (a wrong key shows as bad data in the items decoded with it)
  bool HaveEntry;
  bool Progress;  // SetTotal/SetCompleted are the job's byte progress
  bool Discard;   // nothing is written: every entry is decoded and counted (the check before freeing)
  bool ReadAll;   // an entry the sink does not write is still decoded (freeing: read as the check did)
  xentry_t E;
  CSinkOutStream *SinkSpec;
  CMyComPtr<ISequentialOutStream> Sink;
  CExtractCallback(): J(NULL), Pw(NULL), Arc(NULL), Archive(NULL), Encrypted(false), AnyEncrypted(false), HaveEntry(false), Progress(true), Discard(false), ReadAll(false), SinkSpec(NULL) {}
  HRESULT DiscardStream(ISequentialOutStream **outStream) {
    CDiscardOutStream *d = new CDiscardOutStream;
    CMyComPtr<ISequentialOutStream> keep = d;
    d->J = J;
    *outStream = keep.Detach();
    return S_OK;
  }
};

Z7_COM7F_IMF(CExtractCallback::SetTotal(UInt64 total)) {
  if (Progress && total) J->bytes_total = total;
  return S_OK;
}

Z7_COM7F_IMF(CExtractCallback::SetCompleted(const UInt64 *done)) {
  if (Progress && done) J->bytes_done = *done;
  if (J->progress) J->progress(J, 0);
  return xj_cancelled(J) ? E_ABORT : S_OK;
}

Z7_COM7F_IMF(CExtractCallback::GetStream(UInt32 index, ISequentialOutStream **outStream, Int32 askMode)) {
  *outStream = NULL;
  HaveEntry = false;
  if (askMode != NArchive::NExtract::NAskMode::kExtract) return S_OK;
  Encrypted = GetBool(Arc, index, kpidEncrypted);
  if (Discard) return xj_cancelled(J) ? E_ABORT : DiscardStream(outStream);
  AString name;
  if (!GetStr(Arc, index, kpidPath, name) || name.IsEmpty()) DefaultItemName(Archive, name);
  int kind = ItemKind(Arc, index);
  UInt64 size = 0;
  GetU64(Arc, index, kpidSize, size);
  int b = xs_begin(J, &E, name.Ptr(), kind, size, ItemMTime(Arc, index));
  if (b == XS_FAIL) return E_ABORT;
  if (b == XS_SKIP) return ReadAll ? DiscardStream(outStream) : S_OK;
  HaveEntry = true;
  SinkSpec = new CSinkOutStream;
  Sink = SinkSpec;
  SinkSpec->J = J;
  SinkSpec->E = &E;
  *outStream = Sink;
  Sink->AddRef();
  return S_OK;
}

Z7_COM7F_IMF(CExtractCallback::PrepareOperation(Int32)) { return S_OK; }

Z7_COM7F_IMF(CExtractCallback::SetOperationResult(Int32 opRes)) {
  using namespace NArchive::NExtract::NOperationResult;
  bool ok = opRes == kOK || opRes == kDataAfterEnd;
  if (!ok && !xj_failed(J)) {
    switch (opRes) {
    case kWrongPassword: xj_fail(J, "fs_extract_wrong_password"); break;
    case kCRCError:
    case kDataError:
      // ZipCrypto has no password check of its own: a wrong one shows as bad data.
      if (Encrypted || (AnyEncrypted && J->has_password))
        xj_fail(J, J->has_password ? "fs_extract_wrong_password" : "fs_extract_password_required");
      else xj_fail(J, "fs_extract_corrupt: %s", opRes == kCRCError ? "CRC" : "data");
      break;
    case kUnsupportedMethod: xj_fail(J, "fs_extract_unsupported_method"); break;
    case kUnexpectedEnd: xj_fail(J, "fs_extract_truncated"); break;
    case kUnavailable: xj_fail(J, "fs_extract_missing_volume"); break;
    case kIsNotArc: xj_fail(J, "fs_extract_not_archive"); break;
    default: xj_fail(J, "fs_extract_corrupt: %d", (int)opRes); break;
    }
  }
  if (HaveEntry) {
    HaveEntry = false;
    xs_end(J, &E, ok && !xj_failed(J));
    Sink.Release();
  }
  return xj_failed(J) ? E_ABORT : S_OK;
}

Z7_COM7F_IMF(CExtractCallback::CryptoGetTextPassword(BSTR *password)) { return Pw->Get(password); }

// The outer stream of a .tar.gz, decoded on its own thread into the pipe.
class CFeedCallback Z7_final:
  public IArchiveExtractCallback,
  public ICryptoGetTextPassword,
  public CMyUnknownImp
{
  Z7_IFACES_IMP_UNK_2(IArchiveExtractCallback, ICryptoGetTextPassword)
  Z7_IFACE_COM7_IMP(IProgress)
public:
  xjob_t *J;
  CPasswordState *Pw;
  CPipe *P;
  Int32 Result;
  CFeedCallback(): J(NULL), Pw(NULL), P(NULL), Result(-1) {}
};

Z7_COM7F_IMF(CFeedCallback::SetTotal(UInt64 total)) {
  if (total) J->bytes_total = total;
  return S_OK;
}

Z7_COM7F_IMF(CFeedCallback::SetCompleted(const UInt64 *done)) {
  if (done) J->bytes_done = *done;
  return S_OK;
}

Z7_COM7F_IMF(CFeedCallback::GetStream(UInt32, ISequentialOutStream **outStream, Int32 askMode)) {
  *outStream = NULL;
  if (askMode != NArchive::NExtract::NAskMode::kExtract) return S_OK;
  CPipeOut *spec = new CPipeOut;
  CMyComPtr<ISequentialOutStream> s = spec;
  spec->P = P;
  *outStream = s.Detach();
  return S_OK;
}

Z7_COM7F_IMF(CFeedCallback::PrepareOperation(Int32)) { return S_OK; }

Z7_COM7F_IMF(CFeedCallback::SetOperationResult(Int32 opRes)) {
  Result = opRes;
  return S_OK;
}

Z7_COM7F_IMF(CFeedCallback::CryptoGetTextPassword(BSTR *password)) { return Pw->Get(password); }

struct CFeedThread {
  IInArchive *Outer;
  CFeedCallback *Cb;
  CPipe *P;
  HRESULT Res;
};

static void *FeedMain(void *arg) {
  CFeedThread *t = (CFeedThread *)arg;
#ifdef __PROSPERO__
  /* The same name as the main thread: unnamed, the process can list as payload.elf. */
  proc_name_set_self("ps5u-extract");
#endif
  UInt32 idx = 0;
  t->Res = t->Outer->Extract(&idx, 1, 0, t->Cb);
  t->P->CloseWrite();
  return NULL;
}

// An HRESULT that ended an open or an extraction, as the job's cause (unless one is set already).
static int Fail(xjob_t *j, HRESULT r, CPasswordState &pw, COpenCallback *ocb) {
  if (xj_failed(j)) return -1;
  if (ocb && ocb->VolumeMissing) return xj_fail(j, "fs_extract_missing_volume: %s", ocb->MissingName.Ptr());
  if (r == E_ABORT && pw.Asked && !j->has_password) return xj_fail(j, "fs_extract_password_required");
  if (r == S_FALSE && pw.Asked && j->has_password) return xj_fail(j, "fs_extract_wrong_password");
  if (r == E_OUTOFMEMORY) return xj_fail(j, "fs_extract_no_memory");
  if (r == E_ABORT && xj_cancelled(j)) return -1;
  if (r == S_FALSE) return xj_fail(j, "fs_extract_not_archive");
  return xj_fail(j, "fs_extract_corrupt: 0x%08x", (unsigned)r);
}

static bool IsTarName(const AString &name, const char *archive) {
  static const char *const tarExts[] = {".tgz", ".tpz", ".tbz", ".tbz2", ".txz", ".tzst"};
  if (name.Len() > 4 && strcasecmp(name.Ptr() + name.Len() - 4, ".tar") == 0) return true;
  size_t n = strlen(archive);
  for (unsigned i = 0; i < sizeof tarExts / sizeof tarExts[0]; i++) {
    size_t e = strlen(tarExts[i]);
    if (n > e && strcasecmp(archive + n - e, tarExts[i]) == 0) return true;
  }
  return false;
}

// Totals and the password check, before anything is written.
static int Totals(xjob_t *j, IInArchive *arc, bool sizesKnown, bool *anyEncrypted, UInt64 *totalOut) {
  UInt32 n = 0;
  if (arc->GetNumberOfItems(&n) != S_OK) return xj_fail(j, "fs_extract_corrupt: items");
  UInt64 total = 0, files = 0;
  bool encrypted = false;
  for (UInt32 i = 0; i < n; i++) {
    if (ItemKind(arc, i) != XE_FILE) continue;
    UInt64 size = 0;
    GetU64(arc, i, kpidSize, size);
    total += size;
    files++;
    encrypted |= GetBool(arc, i, kpidEncrypted);
  }
  *anyEncrypted = encrypted;
  if (encrypted && !j->has_password) return xj_fail(j, "fs_extract_password_required");
  j->files_total = files;
  if (sizesKnown) j->unpacked = total;
  if (sizesKnown) j->bytes_total = total;
  *totalOut = total;
  return 0;
}

// Totals, the password check and the space check, before anything is written. Then the folder.
static int Prepare(xjob_t *j, IInArchive *arc, bool sizesKnown, bool *anyEncrypted) {
  UInt64 total = 0;
  if (Totals(j, arc, sizesKnown, anyEncrypted, &total) != 0) return -1;
  if (sizesKnown) {
    // A merge may skip files it already has, so it keeps the per-file check only (as xrar.c does):
    // checked for the whole, a second run over a mostly extracted folder was refused for room it
    // would not use.
    if (!j->merge && xs_check_space(j, total) != 0) return -1;
  }
  return xs_open_dest(j);
}

static int ExtractAll(xjob_t *j, IInArchive *arc, CPasswordState &pw, bool anyEncrypted) {
  CExtractCallback *spec = new CExtractCallback;
  CMyComPtr<IArchiveExtractCallback> cb = spec;
  spec->J = j;
  spec->Pw = &pw;
  spec->Arc = arc;
  spec->Archive = j->archive;
  spec->AnyEncrypted = anyEncrypted || pw.Asked;
  HRESULT r = arc->Extract(NULL, (UInt32)(Int32)-1, 0, cb);
  if (spec->HaveEntry) xs_end(j, &spec->E, 0);
  if (r != S_OK || xj_failed(j)) return Fail(j, r == S_OK ? E_FAIL : r, pw, NULL);
  return 0;
}

// x.tar.gz and the like: `outer` holds one stream, a tar read in order as it is decoded.
static int ExtractTarIn(xjob_t *j, IInArchive *outer, CPasswordState &pw) {
  const CArcInfo *tarInfo = FindArc("tar");
  if (!tarInfo) return xj_fail(j, "fs_extract_unsupported_method: tar");
  CMyComPtr<IInArchive> tar = tarInfo->CreateInArchive();
  CMyComPtr<IArchiveOpenSeq> seq;
  tar.QueryInterface(IID_IArchiveOpenSeq, &seq);
  if (!seq) return xj_fail(j, "fs_extract_unsupported_method: tar");

  CPipe pipe(1u << 20);
  CPipeIn *inSpec = new CPipeIn;
  CMyComPtr<ISequentialInStream> in = inSpec;
  inSpec->P = &pipe;
  CFeedCallback *feedSpec = new CFeedCallback;
  CMyComPtr<IArchiveExtractCallback> feed = feedSpec;
  feedSpec->J = j;
  feedSpec->Pw = &pw;
  feedSpec->P = &pipe;
  CFeedThread t = {outer, feedSpec, &pipe, S_OK};
  pthread_t th;
  pthread_attr_t attr;
  pthread_attr_init(&attr);
  pthread_attr_setstacksize(&attr, 512u << 10);
  if (pthread_create(&th, &attr, FeedMain, &t) != 0) {
    pthread_attr_destroy(&attr);
    return xj_fail(j, "fs_extract_no_memory: thread");
  }
  pthread_attr_destroy(&attr);

  int rc = 0;
  HRESULT r = seq->OpenSeq(in);
  if (r != S_OK) {
    rc = xj_failed(j) ? -1 : xj_fail(j, "fs_extract_not_archive: tar");
  } else if (xs_open_dest(j) != 0) {
    rc = -1;
  } else {
    CExtractCallback *spec = new CExtractCallback;
    CMyComPtr<IArchiveExtractCallback> cb = spec;
    spec->J = j;
    spec->Pw = &pw;
    spec->Arc = tar;
    spec->Archive = j->archive;
    spec->Progress = false; // the feeder's progress (the outer stream's bytes) is the job's
    r = tar->Extract(NULL, (UInt32)(Int32)-1, 0, cb);
    if (spec->HaveEntry) xs_end(j, &spec->E, 0);
    if (r != S_OK || xj_failed(j)) rc = Fail(j, r == S_OK ? E_FAIL : r, pw, NULL);
  }
  // Drain and end the feeder, whatever happened to the tar side.
  pipe.CloseRead();
  pthread_join(th, NULL);
  tar->Close();
  if (rc == 0 && feedSpec->Result != NArchive::NExtract::NOperationResult::kOK &&
      feedSpec->Result != NArchive::NExtract::NOperationResult::kDataAfterEnd) {
    switch (feedSpec->Result) {
    case NArchive::NExtract::NOperationResult::kUnexpectedEnd: rc = xj_fail(j, "fs_extract_truncated"); break;
    case NArchive::NExtract::NOperationResult::kUnsupportedMethod: rc = xj_fail(j, "fs_extract_unsupported_method"); break;
    default: rc = xj_fail(j, "fs_extract_corrupt: %d", (int)feedSpec->Result); break;
    }
  }
  if (rc == 0 && t.Res != S_OK && !xj_failed(j)) rc = Fail(j, t.Res, pw, NULL);
  return rc;
}

// ---- freeing the archive as it is unpacked (xfree.h) ----

static COpenCallback *g_ocb; // the job's open callback: the archive's parts
static UInt64 g_setSize;     // a split set read as one stream: its size (0: not a split set)

static void DropVol(void *ctx, unsigned vol) {
  COpenCallback *o = (COpenCallback *)ctx;
  if (vol < o->Vols.Size()) o->Vols[vol]->Drop();
}

// The archive's parts as xfree takes them. 0, or -1 (cause set).
static int LoadParts(xjob_t *j, xfvols_t *v) {
  unsigned n = g_ocb->VolPaths.Size();
  char **paths = (char **)calloc(n ? n : 1, sizeof *paths);
  if (!paths) return xj_fail(j, "fs_extract_no_memory");
  for (unsigned i = 0; i < n; i++) paths[i] = (char *)g_ocb->VolPaths[i].Ptr();
  int rc = xf_load(j, v, paths, n);
  free(paths);
  if (rc == 0) {
    v->drop = DropVol;
    v->drop_ctx = g_ocb;
  }
  return rc;
}

// The units an archive decodes in, each on its own: a 7z's blocks (the items with no data first),
// else every item. Unit u is order[ustart[u] .. ustart[u+1]); ubytes[u] is what its files hold,
// upack[u] its packed size where the listing gives one.
struct CUnits {
  UInt32 *order;
  size_t *ustart;
  uint64_t *ubytes, *upack;
  size_t n, nu;
  CUnits(): order(NULL), ustart(NULL), ubytes(NULL), upack(NULL), n(0), nu(0) {}
  ~CUnits() {
    free(order);
    free(ustart);
    free(ubytes);
    free(upack);
  }
};

static int BuildUnits(xjob_t *j, IInArchive *arc, bool sevenZ, CUnits &u) {
  UInt32 n = 0;
  if (arc->GetNumberOfItems(&n) != S_OK) return xj_fail(j, "fs_extract_corrupt: items");
  u.n = n;
  u.order = (UInt32 *)malloc((n + 1) * sizeof *u.order);
  u.ustart = (size_t *)malloc((n + 2) * sizeof *u.ustart);
  u.ubytes = (uint64_t *)calloc(n + 1, sizeof *u.ubytes);
  u.upack = (uint64_t *)calloc(n + 1, sizeof *u.upack);
  UInt32 *blk = (UInt32 *)calloc(n + 1, sizeof *blk);
  if (!u.order || !u.ustart || !u.ubytes || !u.upack || !blk) {
    free(blk);
    return xj_fail(j, "fs_extract_no_memory");
  }
  UInt32 maxb = 0;
  for (UInt32 i = 0; i < n; i++) {
    NWindows::NCOM::CPropVariant p;
    blk[i] = sevenZ ? 0 : i + 1;
    if (sevenZ && arc->GetProperty(i, kpidBlock, &p) == S_OK && p.vt == VT_UI4 && p.ulVal < n) blk[i] = p.ulVal + 1;
    if (blk[i] > maxb) maxb = blk[i];
  }
  // Items by unit (a counting sort: each unit's items stay in index order, as 7-Zip wants them).
  size_t *count = (size_t *)calloc((size_t)maxb + 2, sizeof *count);
  if (!count) {
    free(blk);
    return xj_fail(j, "fs_extract_no_memory");
  }
  for (UInt32 i = 0; i < n; i++) count[blk[i] + 1]++;
  for (UInt32 b = 1; b <= maxb + 1; b++) count[b] += count[b - 1];
  for (UInt32 i = 0; i < n; i++) u.order[count[blk[i]]++] = i;
  u.nu = 0;
  for (size_t k = 0; k < n; k++) {
    UInt32 i = u.order[k];
    if (k == 0 || blk[u.order[k - 1]] != blk[i]) u.ustart[u.nu++] = k;
    UInt64 v = 0;
    if (ItemKind(arc, i) == XE_FILE && GetU64(arc, i, kpidSize, v)) u.ubytes[u.nu - 1] += v;
    v = 0;
    if (GetU64(arc, i, kpidPackSize, v)) u.upack[u.nu - 1] += v;
  }
  u.ustart[u.nu] = n;
  free(count);
  free(blk);
  return 0;
}

// One unit, decoded: written through the sink, or (`discard`) only counted. Every entry is decoded
// either way, so the archive is read the same both times.
static int RunUnit(xjob_t *j, IInArchive *arc, CPasswordState &pw, bool anyEncrypted, const UInt32 *idx, UInt32 cnt,
                   bool discard) {
  CExtractCallback *spec = new CExtractCallback;
  CMyComPtr<IArchiveExtractCallback> cb = spec;
  spec->J = j;
  spec->Pw = &pw;
  spec->Arc = arc;
  spec->Archive = j->archive;
  spec->AnyEncrypted = anyEncrypted || pw.Asked;
  spec->Progress = false; // the job counts its own bytes: the check, then the writes
  spec->Discard = discard;
  spec->ReadAll = true;
  HRESULT r = arc->Extract(idx, cnt, 0, cb);
  if (spec->HaveEntry) xs_end(j, &spec->E, 0);
  if (g_track.BadRead && !xj_failed(j)) xj_fail(j, "fs_extract_corrupt: a deleted part was read");
  if (r != S_OK || xj_failed(j)) return Fail(j, r == S_OK ? E_FAIL : r, pw, NULL);
  return 0;
}

// Extract with the archive's space given back as it goes. A split set of a 7z, zip or tar in more
// than one unit is freed part by part (the check first); anything else is deleted at the end.
static int ExtractFreeing(xjob_t *j, IInArchive *arc, const CArcInfo *info, CPasswordState &pw) {
  xfvols_t v;
  CUnits units;
  uint64_t *lo = NULL, *need = NULL;
  UInt64 total = 0;
  bool anyEncrypted = false, parts;
  int rc = -1;
  if (LoadParts(j, &v) != 0) return -1;
  j->same_drive = xf_same_drive(j->archive, j->dest);
  parts = g_setSize && v.count >= 2 && g_setSize == v.total;
  if (parts && BuildUnits(j, arc, strcasecmp(info->Name, "7z") == 0, units) != 0) goto out;
  if (parts && units.nu < 2) parts = false;
  if (!parts) {
    snprintf(j->free_mode, sizeof j->free_mode, "end");
    j->durable = 1;
    if (Prepare(j, arc, true, &anyEncrypted) != 0) goto out;
    j->free_peak = j->unpacked;
    j->plan_known = 1;
    rc = ExtractAll(j, arc, pw, anyEncrypted);
    if (rc == 0) rc = xf_finish(j, &v);
    goto out;
  }
  snprintf(j->free_mode, sizeof j->free_mode, "parts");
  if (Totals(j, arc, true, &anyEncrypted, &total) != 0) goto out;
  // Certainly too little room, even with every part given back: said before the long read.
  if (!j->merge && (j->same_drive ? (total > v.total && xs_check_space(j, total - v.total) != 0)
                                  : xs_check_space(j, total) != 0))
    goto out;
  lo = (uint64_t *)malloc(units.nu * sizeof *lo);
  need = (uint64_t *)malloc(units.nu * sizeof *need);
  if (!lo || !need) {
    xj_fail(j, "fs_extract_no_memory");
    goto out;
  }
  // The check: every unit read through once with the archive whole, nothing written, noting where
  // each one starts reading. Damage anywhere is found here, before a part is deleted.
  j->checking = 1;
  j->bytes_total = total * 2;
  j->bytes_done = 0;
  g_track.VStart = v.start;
  g_track.BadRead = false;
  if (j->progress) j->progress(j, 1);
  for (size_t u = 0; u < units.nu; u++) {
    g_track.Reset();
    if (RunUnit(j, arc, pw, anyEncrypted, units.order + units.ustart[u], (UInt32)(units.ustart[u + 1] - units.ustart[u]),
                true) != 0)
      goto out;
    lo[u] = g_track.Any ? g_track.Lo : XF_NO_READ;
  }
  j->checking = 0;
  xf_need(lo, units.nu, need);
  j->free_peak = j->same_drive ? xf_peak(&v, need, units.ubytes, units.nu) : total;
  j->plan_known = 1;
  // Exact now: the room the unpack takes at its largest, with the parts given back on the way.
  if (!j->merge && xs_check_space(j, j->free_peak) != 0) goto out;
  if (xs_open_dest(j) != 0) goto out;
  j->durable = 1;
  j->count_written = 1;
  j->bytes_done = total;
  for (size_t u = 0; u < units.nu; u++) {
    if (xf_release(j, &v, xf_before(&v, need[u])) != 0) goto stopped;
    g_track.Reset();
    if (RunUnit(j, arc, pw, anyEncrypted, units.order + units.ustart[u], (UInt32)(units.ustart[u + 1] - units.ustart[u]),
                false) != 0)
      goto stopped;
  }
  rc = xf_finish(j, &v);
  if (rc == 0) goto out;
stopped:
  rc = -1;
  xf_abandon(j, &v);
out:
  j->checking = 0;
  g_track.VStart = NULL;
  free(lo);
  free(need);
  xf_free_vols(&v);
  return rc;
}


// A single compressed stream (x.gz, x.tar.gz): nothing comes back until the end.
static int ExtractFreeingStream(xjob_t *j, IInArchive *arc, CPasswordState &pw, bool tarIn) {
  xfvols_t v;
  int rc;
  if (LoadParts(j, &v) != 0) return -1;
  j->same_drive = xf_same_drive(j->archive, j->dest);
  snprintf(j->free_mode, sizeof j->free_mode, "end");
  j->durable = 1;
  if (tarIn) {
    rc = ExtractTarIn(j, arc, pw);
  } else {
    bool anyEncrypted = false;
    rc = Prepare(j, arc, false, &anyEncrypted);
    if (rc == 0) rc = ExtractAll(j, arc, pw, anyEncrypted);
  }
  if (rc == 0) rc = xf_finish(j, &v);
  xf_free_vols(&v);
  return rc;
}

// A listing that asked about freeing: how this archive would be freed and the room it would take,
// from the listing alone (nothing is decoded). Never fails the listing: an archive that can't be
// freed says why in free_why.
static void PlanFromListing(xjob_t *j, IInArchive *arc, const CArcInfo *info, bool stream) {
  xjob_t *probe = (xjob_t *)calloc(1, sizeof *probe);
  xfvols_t v;
  CUnits units;
  if (!probe) return;
  if (LoadParts(probe, &v) != 0) {
    snprintf(j->free_why, sizeof j->free_why, "%s", probe->cause);
    free(probe);
    return;
  }
  j->parts = v.count;
  j->archive_bytes = v.total;
  j->same_drive = xf_same_drive(j->archive, j->dest);
  bool sevenZ = strcasecmp(info->Name, "7z") == 0, zip = strcasecmp(info->Name, "zip") == 0;
  bool listed = !stream && BuildUnits(probe, arc, sevenZ, units) == 0;
  UInt64 total = 0;
  for (size_t u = 0; listed && u < units.nu; u++) total += units.ubytes[u];
  j->unpacked = listed ? total : 0;
  if (!listed || !g_setSize || v.count < 2 || g_setSize != v.total || units.nu < 2) {
    snprintf(j->free_mode, sizeof j->free_mode, "end");
    j->free_peak = j->unpacked;
    j->plan_known = listed;
  } else {
    // Where each unit starts: a zip's own offsets, a 7z's blocks packed one after another behind
    // its 32-byte start header, a tar's items one after another. An estimate for the plan shown
    // before it starts (the unpack measures it while the archive is still whole).
    uint64_t *lo = (uint64_t *)malloc(units.nu * sizeof *lo), *need = (uint64_t *)malloc(units.nu * sizeof *need);
    if (lo && need) {
      uint64_t at = sevenZ ? 32 : 0;
      for (size_t u = 0; u < units.nu; u++) {
        UInt32 first = units.order[units.ustart[u]];
        UInt64 off = 0;
        if (zip) lo[u] = GetU64(arc, first, kpidOffset, off) ? off : at;
        else if (sevenZ && !units.upack[u]) lo[u] = XF_NO_READ; // folders and empty files
        else lo[u] = at;
        at += units.upack[u] + (sevenZ || zip ? 0 : 512);
        if (!sevenZ && !zip) at = (at + 511) & ~(uint64_t)511;
      }
      xf_need(lo, units.nu, need);
      snprintf(j->free_mode, sizeof j->free_mode, "parts");
      j->free_peak = j->same_drive ? xf_peak(&v, need, units.ubytes, units.nu) : total;
      j->plan_known = 1;
    }
    free(lo);
    free(need);
  }
  xf_free_vols(&v);
  free(probe);
}

static const char *const kStreamFormats[] = {"gzip", "bzip2", "xz", "zstd", "lzma"};


static int ExtractOpened(xjob_t *j, IInArchive *arc, const CArcInfo *info, CPasswordState &pw, COpenCallback *ocb,
                         int depth);

// A split set (x.7z.001): the joined volumes, opened as the archive they make up.
static int ExtractSplit(xjob_t *j, IInArchive *split, CPasswordState &pw, COpenCallback *ocb, int depth) {
  CMyComPtr<IInArchiveGetStream> gs;
  split->QueryInterface(IID_IInArchiveGetStream, (void **)&gs);
  if (!gs) return xj_fail(j, "fs_extract_not_archive");
  CMyComPtr<ISequentialInStream> seq;
  if (gs->GetStream(0, &seq) != S_OK || !seq) return Fail(j, E_FAIL, pw, ocb);
  CMyComPtr<IInStream> joined;
  seq.QueryInterface(IID_IInStream, &joined);
  if (!joined) return xj_fail(j, "fs_extract_not_archive");
  {
    // The set's size, read as one stream: its parts are where xfree expects them only when it is
    // the sum of the files opened.
    UInt64 size = 0;
    if (joined->Seek(0, STREAM_SEEK_END, &size) == S_OK) g_setSize = size;
    if (joined->Seek(0, STREAM_SEEK_SET, NULL) != S_OK) return Fail(j, E_FAIL, pw, ocb);
  }
  AString inner;
  if (!GetStr(split, 0, kpidPath, inner) || inner.IsEmpty()) DefaultItemName(j->archive, inner);
  if (inner.Len() > 4 && strcasecmp(inner.Ptr() + inner.Len() - 4, ".rar") == 0)
    return xj_fail(j, "fs_extract_unsupported_method: rar in a numbered split set");
  CMyComPtr<IInArchive> arc;
  const CArcInfo *used = NULL;
  HRESULT r = OpenAny(inner.Ptr(), joined, ocb, arc, &used);
  if (r != S_OK || !arc) return Fail(j, r, pw, ocb);
  int rc = ExtractOpened(j, arc, used, pw, ocb, depth + 1);
  arc->Close();
  return rc;
}

static int ExtractOpened(xjob_t *j, IInArchive *arc, const CArcInfo *info, CPasswordState &pw, COpenCallback *ocb,
                         int depth) {
  if (strcmp(info->Name, "Split") == 0) {
    if (depth > 0) return xj_fail(j, "fs_extract_not_archive");
    return ExtractSplit(j, arc, pw, ocb, depth);
  }
  bool stream = false;
  for (unsigned k = 0; k < sizeof kStreamFormats / sizeof kStreamFormats[0]; k++)
    stream |= strcasecmp(info->Name, kStreamFormats[k]) == 0;
  if (stream) {
    AString inner;
    if (!GetStr(arc, 0, kpidPath, inner) || inner.IsEmpty()) DefaultItemName(j->archive, inner);
    if (IsTarName(inner, j->archive)) {
      if (j->list) {
        int rc = ExtractTarIn(j, arc, pw);
        if (rc == 0 && j->free_req) PlanFromListing(j, arc, info, true);
        return rc;
      }
      if (j->free_req) return ExtractFreeingStream(j, arc, pw, true);
      return ExtractTarIn(j, arc, pw);
    }
  }
  if (j->list) {
    // Names only: the items' headers, nothing decoded (a solid archive would decode everything).
    UInt32 n = 0;
    if (arc->GetNumberOfItems(&n) != S_OK) return xj_fail(j, "fs_extract_corrupt: items");
    j->files_total = n;
    for (UInt32 i = 0; i < n; i++) {
      AString name;
      if (!GetStr(arc, i, kpidPath, name) || name.IsEmpty()) DefaultItemName(j->archive, name);
      xentry_t e;
      if (xs_begin(j, &e, name.Ptr(), ItemKind(arc, i), 0, -1) == XS_FAIL) return -1;
    }
    if (j->free_req) PlanFromListing(j, arc, info, stream);
    return 0;
  }
  if (j->free_req) return stream ? ExtractFreeingStream(j, arc, pw, false) : ExtractFreeing(j, arc, info, pw);
  // A stream format's one item has no reliable size (gzip keeps it modulo 4 GiB): checked per write.
  bool anyEncrypted = false;
  if (Prepare(j, arc, !stream, &anyEncrypted) != 0) return -1;
  return ExtractAll(j, arc, pw, anyEncrypted);
}

extern "C" int x7z_extract(xjob_t *j) {
  try {
    CPasswordState pw(j);
    CFdInStream *spec = new CFdInStream;
    CMyComPtr<IInStream> stream = spec;
    if (!spec->Open(j->archive)) return xj_fail(j, "fs_extract_open_failed: %s", strerror(errno));
    COpenCallback *ocbSpec = new COpenCallback;
    CMyComPtr<IArchiveOpenCallback> ocb = ocbSpec;
    ocbSpec->J = j;
    ocbSpec->Pw = &pw;
    ocbSpec->AddVol(spec, AString(j->archive));
    g_ocb = ocbSpec;
    g_setSize = 0;
    const char *slash = strrchr(j->archive, '/');
    ocbSpec->Dir.SetFrom(j->archive, (unsigned)(slash - j->archive + 1));
    Utf8ToU(slash + 1, ocbSpec->FirstName);

    CMyComPtr<IInArchive> arc;
    const CArcInfo *used = NULL;
    HRESULT r = OpenAny(slash + 1, stream, ocb, arc, &used);
    if (r != S_OK || !arc) return Fail(j, r, pw, ocbSpec);
    int rc = ExtractOpened(j, arc, used, pw, ocbSpec, 0);
    arc->Close();
    g_ocb = NULL;
    return rc;
  } catch (...) {
    g_ocb = NULL;
    g_track.VStart = NULL;
    return xj_fail(j, "fs_extract_no_memory");
  }
}
