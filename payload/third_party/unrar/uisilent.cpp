#include "rar.hpp"
// Purely user interface function. Gets and returns user input.
UIASKREP_RESULT uiAskReplace(std::wstring &Name,int64 FileSize,RarTime *FileTime,uint Flags)
{
  RAR_UNUSED(Name);
  RAR_UNUSED(FileSize);
  RAR_UNUSED(FileTime);
  RAR_UNUSED(Flags);
  return UIASKREP_R_REPLACE;
}




void uiStartArchiveExtract(bool Extract,const std::wstring &ArcName)
{
  RAR_UNUSED(Extract);
  RAR_UNUSED(ArcName);
}


bool uiStartFileExtract(const std::wstring &FileName,bool Extract,bool Test,bool Skip)
{
  RAR_UNUSED(FileName);
  RAR_UNUSED(Extract);
  RAR_UNUSED(Test);
  RAR_UNUSED(Skip);
  return true;
}


void uiExtractProgress(int64 CurFileSize,int64 TotalFileSize,int64 CurSize,int64 TotalSize)
{
  RAR_UNUSED(CurFileSize);
  RAR_UNUSED(TotalFileSize);
  RAR_UNUSED(CurSize);
  RAR_UNUSED(TotalSize);
}


void uiProcessProgress(const char *Command,int64 CurSize,int64 TotalSize)
{
  RAR_UNUSED(Command);
  RAR_UNUSED(CurSize);
  RAR_UNUSED(TotalSize);
}


void uiMsgStore::Msg()
{
}


bool uiGetPassword(UIPASSWORD_TYPE Type,const std::wstring &FileName,
                   SecPassword *Password,CheckPassword *CheckPwd)
{
  RAR_UNUSED(Type);
  RAR_UNUSED(FileName);
  RAR_UNUSED(Password);
  RAR_UNUSED(CheckPwd);
  return false;
}


bool uiIsGlobalPasswordSet()
{
  return false;
}


void uiAlarm(UIALARM_TYPE Type)
{
  RAR_UNUSED(Type);
}


bool uiIsAborted()
{
  return false;
}


void uiGiveTick()
{
}


bool uiDictLimit(CommandData *Cmd,const std::wstring &FileName,uint64 DictSize,uint64 MaxDictSize)
{
  RAR_UNUSED(FileName);
#ifdef RARDLL
  if (Cmd->Callback!=nullptr &&
      Cmd->Callback(UCM_LARGEDICT,Cmd->UserData,(LPARAM)(DictSize/1024),(LPARAM)(MaxDictSize/1024))==1)
    return true; // Continue extracting if unrar.dll callback permits it.
#endif
  return false; // Stop extracting.
}


#ifndef SFX_MODULE
const wchar *uiGetMonthName(uint Month)
{
  RAR_UNUSED(Month);
  return L"";
}


const wchar *uiGetWeekDayName(uint Day)
{
  RAR_UNUSED(Day);
  return L"";
}
#endif


void uiEolAfterMsg()
{
}
