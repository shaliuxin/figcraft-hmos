@echo off
"C:\Program Files\Huawei\DevEco Studio\sdk\default\openharmony\native\llvm\bin\clang.exe" -target x86_64-linux-ohos --sysroot="C:\Program Files\Huawei\DevEco Studio\sdk\default\openharmony\native\sysroot" -D__MUSL__ %*
