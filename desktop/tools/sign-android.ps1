<#
  给 Android 侧载包签名。

  用法：
    powershell -ExecutionPolicy Bypass -File .\sign-android.ps1 -Version 0.6.3

  它做三件事：
    1. 首次运行时生成签名密钥（.keystore），密码是随机生成的，只写在你自己电脑上
    2. 用 zipalign 对齐未签名包
    3. 用 apksigner 签出可以安装的 APK，并验签

  为什么需要固定密钥：Android 判断「新包能不能覆盖升级旧包」比的是签名。
  密钥一换，手机上就得先卸载旧版再装新版，而卸载会连数据一起没。
#>

param(
    # 版本号，只用来给输出文件起名
    [string]$Version = "0.6.3",
    # 未签名 APK 的路径；留空则用 tauri 默认的 universal release 包
    [string]$UnsignedApk = ""
)

$ErrorActionPreference = 'Stop'

# ---- 路径 ----
$Project    = Split-Path -Parent $PSScriptRoot      # ...\desktop
$TauriDir   = Join-Path $Project "src-tauri"
$GenAndroid = Join-Path $TauriDir "gen\android"
$BuildTools = "$env:LOCALAPPDATA\Android\Sdk\build-tools\34.0.0"
$KeyDir     = "E:\个人工作台-密钥"
$JdkHome    = "E:\个人工作台-工具\jdk\jdk-17.0.20.1+1"
$JdkBin     = Join-Path $JdkHome "bin"

# apksigner 是个 .bat，靠 JAVA_HOME / PATH 找 java。
# 我们把 JDK 解压在 E 盘工具目录里、没装进系统，所以这里必须显式指给它，
# 不然它会报 "JAVA_HOME is not set and no 'java' command could be found"。
if (Test-Path (Join-Path $JdkBin "java.exe")) {
    $env:JAVA_HOME = $JdkHome
    $env:PATH = "$JdkBin;$env:PATH"
} else {
    throw "找不到 JDK：$JdkBin\java.exe"
}

if (-not $UnsignedApk) {
    $UnsignedApk = Join-Path $GenAndroid "app\build\outputs\apk\universal\release\app-universal-release-unsigned.apk"
}
if (-not (Test-Path $UnsignedApk)) { throw "找不到未签名 APK：$UnsignedApk" }
if (-not (Test-Path $BuildTools)) { throw "找不到 build-tools：$BuildTools" }
if (-not (Test-Path $KeyDir)) { New-Item -ItemType Directory -Force -Path $KeyDir | Out-Null }

$Keystore = Join-Path $KeyDir "workbench.keystore"
# 密码文件路径。注意变量名要跟下面用的一模一样：
# $PwdFile 和 $PwFile 是**两个不同的变量**（一个带 d 一个不带），不是大小写差异。
# 写错过一次，Set-Content -Path $PwFile 直接报 "because it is null"，白生成一个没密码的密钥。
$PassFile = Join-Path $KeyDir "android-keystore-密码.txt"
$Alias    = "workbench"
$OutDir   = Join-Path $Project "..\发布-安卓"
$OutApk   = Join-Path $OutDir "PersonalWorkbench_$Version.apk"
$TmpDir   = Join-Path $env:TEMP "apk-sign"
New-Item -ItemType Directory -Force -Path $OutDir, $TmpDir | Out-Null

# ---- 密钥：没有就生成，有就直接读密码 ----
if (-not (Test-Path $Keystore)) {
    Write-Host "首次运行：生成签名密钥 ..." -ForegroundColor Cyan

    # 随机强密码。两个 GUID 拼起来 = 64 位十六进制。
    # 特意只用十六进制字符：省掉所有命令行转义的麻烦（坑见 方案.md）。
    $pw = [Guid]::NewGuid().ToString("N") + [Guid]::NewGuid().ToString("N")
    $env:KS_PASS = $pw

    # 有效期 10950 天 = 30 年。Android 要求签名有效期至少到 2033 年以后，
    # 30 年是为了以后换新电脑重签也不至于过期。
    & "$JdkBin\keytool.exe" -genkeypair -v `
        -keystore $Keystore -storetype PKCS12 `
        -alias $Alias -keyalg RSA -keysize 2048 -validity 10950 `
        -storepass:env KS_PASS -keypass:env KS_PASS `
        -dname "CN=Personal Workbench, OU=Self Signed, O=Personal, C=CN"
    if ($LASTEXITCODE -ne 0) { throw "生成密钥失败" }

    @"
个人工作台 · Android 签名密钥密码
====================================

密钥文件：$Keystore
别名    ：$Alias
有效期  ：30 年（到 2056 年）

密码    ：$pw

⚠️ 这个密钥丢了 / 忘了，手机上就得先卸载旧版再装新版，数据会没。
⚠️ 这个密钥泄露了，别人就能做出「看起来跟你的一模一样」的安装包。
   所以它和 updater.key 一样，放在仓库外面，不要提交到 git。

以后重新签名 APK（脚本会自动读这个密码）：
    powershell -ExecutionPolicy Bypass -File .\sign-android.ps1 -Version <版本号>
"@ | Set-Content -Path $PassFile -Encoding UTF8

    Write-Host "已生成，密码写在：$PassFile" -ForegroundColor Green
} else {
    Write-Host "使用已有密钥：$Keystore" -ForegroundColor Cyan
    if (-not (Test-Path $PassFile)) { throw "密钥在，但密码文件没了：$PassFile" }
    # 从密码文件里读回密码：找以「密码」开头的那一行，取全角冒号后面的内容
    $pw = ((Get-Content $PassFile -Encoding UTF8 | Where-Object { $_ -like "密码*" }) -split "：")[-1].Trim()
    if (-not $pw) { throw "密码文件格式不对，读不出密码：$PassFile" }
    $env:KS_PASS = $pw
}

# ---- 对齐 ----
# zipalign 是 Android 官方要求的内存布局优化，必须在签名之前做
$Aligned = Join-Path $TmpDir "aligned.apk"
& "$BuildTools\zipalign.exe" -f -p 4 $UnsignedApk $Aligned
if ($LASTEXITCODE -ne 0) { throw "zipalign 失败" }

# ---- 签名 ----
& "$BuildTools\apksigner.bat" sign `
    --ks $Keystore --ks-key-alias $Alias `
    --ks-pass env:KS_PASS --key-pass env:KS_PASS `
    --out $OutApk $Aligned
if ($LASTEXITCODE -ne 0) { throw "签名失败" }

# ---- 验签（不信「应该成功」，当场验一遍）----
Write-Host ""
Write-Host "验签结果：" -ForegroundColor Cyan
& "$BuildTools\apksigner.bat" verify --verbose --print-certs $OutApk
if ($LASTEXITCODE -ne 0) { throw "验签没过" }

Write-Host ""
Write-Host ("完成：{0}  {1:N0} 字节" -f $OutApk, (Get-Item $OutApk).Length) -ForegroundColor Green

# 清掉临时文件里的明文密码副本
$env:KS_PASS = $null
