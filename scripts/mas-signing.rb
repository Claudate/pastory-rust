#!/usr/bin/env ruby
# 用 App Store Connect 官方接口做三件 fastlane 命令行不做的事：
#   1) 查/建 Bundle ID（App ID）
#   2) 建签名证书（本地生成私钥与 CSR，证书直接导入登录钥匙串）
#   3) 建 Mac App Store 类型的 provisioning profile
#
# 为什么不用现成的 fastlane 命令：produce/sigh 的 macOS 分支要么走 Apple ID 会话，
# 要么根本不支持 installer 证书；而干净 Mac 上最稳的是 API Key（官方接口、无 2FA）。
# 这个文件只做「接口调用 + 落盘 + 导入」，不判断该不该做——那由 mas-auto.sh 决定。
#
# 用法（通常由 mas-auto.sh 调用）：
#   ruby mas-signing.rb list --json
#   ruby mas-signing.rb ensure-bundle-id
#   ruby mas-signing.rb ensure-app
#   ruby mas-signing.rb create-cert --type DISTRIBUTION --out-dir DIR
#   ruby mas-signing.rb create-profile --name "PasteJar MAS" --out PATH
#
# 认证二选一：
#   A) ASC_KEY_ID + ASC_ISSUER_ID + ~/.appstoreconnect/private_keys/AuthKey_<id>.p8
#   B) FASTLANE_USER + FASTLANE_PASSWORD（Apple ID，2FA 由 fastlane 弹）
#
# 所有模式都往 stdout 打一行 JSON；失败时 {"ok":false,"error":...,"hint":...} 并 exit 2。
require 'json'
require 'openssl'
require 'base64'
require 'fileutils'
require 'tmpdir'

BUNDLE_ID = ENV['BUNDLE_ID'] || 'com.cici.snipclip'
APP_NAME  = ENV['ASC_APP_NAME'] || 'PasteJar'
PROFILE_TYPE = 'MAC_APP_STORE'

def emit(obj, code = 0)
  puts JSON.generate(obj)
  exit code
end

def die!(msg, hint = nil)
  # 同一个 JSON 同时写 stderr：调用方常把 stdout 收进变量（$(...)），
  # 只写 stdout 时失败原因就被吞了，日志上只剩「看上面 error/hint」。
  line = JSON.generate({ 'ok' => false, 'error' => msg, 'hint' => hint })
  warn line
  emit({ 'ok' => false, 'error' => msg, 'hint' => hint }, 2)
end

begin
  require 'spaceship'
rescue LoadError
  die!('找不到 fastlane 的 spaceship 库',
       '用 mas-auto.sh 调本脚本（它会设置 GEM_HOME），或先 brew install fastlane')
end

def api_creds?
  !ENV['ASC_KEY_ID'].to_s.empty? && !ENV['ASC_ISSUER_ID'].to_s.empty?
end

def api_key_path
  return ENV['ASC_KEY_PATH'] if ENV['ASC_KEY_PATH'].to_s != ''
  id = ENV['ASC_KEY_ID'].to_s
  die!('没有 ASC_KEY_ID',
       'ASC → 用户和访问 → 集成 → App Store Connect API → 生成密钥（App Manager）') if id.empty?
  p = File.expand_path("~/.appstoreconnect/private_keys/AuthKey_#{id}.p8")
  unless File.exist?(p)
    die!("找不到私钥 #{p}",
         "把下载的 AuthKey_#{id}.p8 放进 ~/.appstoreconnect/private_keys/（文件名要和 Key ID 对上）")
  end
  p
end

# API Key 优先；没有 Key 时用 Apple ID 会话（fresh 安装的机器上 Key 可能还没建）。
def connect!
  if api_creds?
    token = Spaceship::ConnectAPI::Token.create(
      key_id: ENV['ASC_KEY_ID'], issuer_id: ENV['ASC_ISSUER_ID'], filepath: api_key_path
    )
    Spaceship::ConnectAPI.token = token
  else
    user = ENV['FASTLANE_USER'].to_s
    pass = ENV['FASTLANE_PASSWORD'].to_s
    if user.empty? || pass.empty?
      die!('没有可用凭据',
           '设置 ASC_KEY_ID/ASC_ISSUER_ID（推荐）或 FASTLANE_USER/FASTLANE_PASSWORD')
    end
    # use_portal 必须为 true：BundleId / Certificate / Profile 全在
    # ConnectAPI 的 provisioning 端点下，而 provisioning_request_client 只有在
    # portal_client 存在时才被赋值（client.rb:195）。用 false 会拿到
    # 「You need to instantiate this module with provisioning_request_client」。
    Spaceship::ConnectAPI.login(user, pass, use_portal: true, use_tunes: true)
  end
rescue StandardError => e
  die!("登录 App Store Connect 失败：#{e.message}",
       '账号需要 Admin 或 App Manager；Apple ID 方式要先过 2FA')
end

def find_bundle_id
  Spaceship::ConnectAPI::BundleId.all.find { |b| b.identifier == BUNDLE_ID }
end

def ensure_bundle_id
  existing = find_bundle_id
  return { 'ok' => true, 'status' => 'exists', 'id' => existing.id } if existing

  created = nil
  errors = []
  # 平台枚举在不同账号/时期接受 MAC_OS 或 UNIVERSAL；先 MAC_OS，失败再试 UNIVERSAL，
  # 两个都失败就把 API 原文一起带出去，而不是笼统说「失败了」。
  %w[MAC_OS UNIVERSAL].each do |platform|
    begin
      created = Spaceship::ConnectAPI::BundleId.create(name: APP_NAME, platform: platform, identifier: BUNDLE_ID)
      break
    rescue StandardError => e
      errors << "#{platform}: #{e.message}"
    end
  end
  die!("建 App ID 失败（#{BUNDLE_ID}）", errors.join(' | ')) unless created
  { 'ok' => true, 'status' => 'created', 'id' => created.id }
end

def find_app
  Spaceship::ConnectAPI::App.find(BUNDLE_ID)
rescue StandardError => e
  die!("查询 ASC app 记录失败：#{e.message}", '若用 API Key，Key 需要有 App Manager 权限')
end

def ensure_app
  app = find_app
  return { 'ok' => true, 'status' => 'exists', 'id' => app.id, 'name' => app.name } if app
  begin
    # App.create 只把 post_app 的响应原样返回（models/app.rb:89 没有 to_models），
    # 拿它当模型取 .id 会 NoMethodError——所以建完一律重新查，且 ASC 有延迟，要轮询。
    Spaceship::ConnectAPI::App.create(
      name: APP_NAME, version_string: '1.0', sku: 'pastejar-mas',
      primary_locale: 'zh-Hans', bundle_id: BUNDLE_ID, platforms: ['MAC_OS']
    )
  rescue StandardError => e
    die!("建 ASC app 记录失败：#{e.message}",
         'ASC 没有公开的「建 app」接口，这条路可能被拒——按 mas-auto 打印的表单在网页建一次，再重跑即可')
  end
  app = nil
  15.times do
    app = find_app
    break if app
    sleep 2
  end
  die!('已在 ASC 建了 app，但查不到记录',
       'App Store Connect 有延迟：等 1 分钟重跑本命令；或去网页确认 app 是否真的建出来了') unless app
  { 'ok' => true, 'status' => 'created', 'id' => app.id, 'name' => app.name }
end

def distribution_cert
  Spaceship::ConnectAPI::Certificate.all.find do |c|
    %w[DISTRIBUTION MAC_APP_DISTRIBUTION].include?(c.certificate_type) && c.valid?
  end
end

# 建证书：本地生成私钥 → CSR → 提交 ASC → 证书与私钥合成 p12 → 导入登录钥匙串。
# 私钥只在本地产生，所以 codesign/productsign 不需要找别的机器要 .p12。
def create_cert(type, out_dir)
  key = OpenSSL::PKey::RSA.new(2048)
  csr = OpenSSL::X509::Request.new
  csr.version = 0
  csr.subject = OpenSSL::X509::Name.parse("/CN=PasteJar #{type} #{BUNDLE_ID}")
  csr.public_key = key.public_key
  csr.sign(key, OpenSSL::Digest::SHA256.new)

  cert = begin
    Spaceship::ConnectAPI::Certificate.create(certificate_type: type, csr_content: csr.to_pem)
  rescue StandardError => e
    msg = e.message
    hint = if msg =~ /maximum number|already have a current/i
             '该团队这类证书已达上限：导入另一台机器导出的 .p12，或去门户吊销一张旧的（会让那台机器的构建失效）。脚本不会替你吊销。'
           elsif msg =~ /not allowed|permission|Forbidden/i
             '账号角色不足：建分发证书需要 Admin（团队管理员）。'
           end
    die!("建证书失败（#{type}）：#{msg}", hint)
  end

  der = Base64.decode64(cert.certificate_content.to_s)
  x509 = OpenSSL::X509::Certificate.new(der)
  FileUtils.mkdir_p(out_dir)
  p12_path = File.join(out_dir, "#{type.downcase}.p12")
  File.binwrite(p12_path, OpenSSL::PKCS12.create('', "PasteJar #{type}", key, x509).to_der)

  cn = x509.subject.to_s[/CN=([^,\/]+)/, 1].to_s
  kc = File.expand_path('~/Library/Keychains/login.keychain-db')
  # -T 把 codesign/productsign/productbuild 预先列为允许使用的程序，
  # -A 让其它工具也能用（构建机上可接受）；否则第一次签名会弹钥匙串授权框，
  # 而干净机器上的人不知道该点哪个按钮。
  imported = system('/usr/bin/security', 'import', p12_path, '-k', kc, '-P', '',
                    '-T', '/usr/bin/codesign', '-T', '/usr/bin/productsign',
                    '-T', '/usr/bin/productbuild', '-A',
                    out: File::NULL, err: File::NULL)
  FileUtils.rm_f(p12_path)
  unless imported
    die!("证书建好了，但导入登录钥匙串失败（#{cn}）",
         '在 Finder 里双击 MAS-上架.command 运行（IDE 的终端不能写钥匙串）；或钥匙串访问里手动导入')
  end

  { 'ok' => true, 'commonName' => cn, 'expires' => x509.not_after.to_s, 'type' => type }
end

def create_profile(name, out_path)
  bid = find_bundle_id
  die!('App ID 还不存在', '先跑 mas-signing.rb ensure-bundle-id') unless bid
  cert = distribution_cert
  die!('没有可用的分发证书', '先跑 mas-signing.rb create-cert --type DISTRIBUTION') unless cert

  # 名字里带证书 id 前缀：这样「同名复用」就等于「同一张证书」，
  # 换证后自然生成新 profile，不会拿到绑旧证书的 profile（上传时会被判签名不符）。
  label = "#{name} #{cert.id[0, 8]}"
  existing = Spaceship::ConnectAPI::Profile.all.find { |p| p.name == label }
  profile = existing
  unless profile
    profile = begin
      Spaceship::ConnectAPI::Profile.create(
        name: label, profile_type: PROFILE_TYPE,
        bundle_id_id: bid.id, certificate_ids: [cert.id]
      )
    rescue StandardError => e
      die!("建 profile 失败：#{e.message}",
           '确认 App ID 与本机分发证书都属于同一团队；Apple ID 方式需要 Admin 权限')
    end
  end

  content = Base64.decode64(profile.profile_content.to_s)
  FileUtils.mkdir_p(File.dirname(out_path))
  File.binwrite(out_path, content)
  { 'ok' => true, 'path' => out_path, 'name' => profile.name, 'uuid' => profile.uuid,
    'reused' => !existing.nil?, 'certId' => cert.id }
end

def list_state
  bid = find_bundle_id
  certs = Spaceship::ConnectAPI::Certificate.all.map do |c|
    { 'id' => c.id, 'type' => c.certificate_type, 'expires' => c.expiration_date, 'name' => c.display_name }
  end
  profiles = Spaceship::ConnectAPI::Profile.all.map do |p|
    { 'id' => p.id, 'name' => p.name, 'type' => p.profile_type, 'uuid' => p.uuid }
  end
  apps = begin
    Spaceship::ConnectAPI::App.all.map { |a| { 'id' => a.id, 'name' => a.name, 'bundleId' => a.bundle_id, 'sku' => a.sku } }
  rescue StandardError
    []
  end
  { 'ok' => true, 'bundleId' => (bid ? { 'id' => bid.id, 'exists' => true } : { 'exists' => false }),
    'certificates' => certs, 'profiles' => profiles, 'apps' => apps }
end

mode = ARGV.shift
opts = {}
while (a = ARGV.shift)
  case a
  when '--json' then opts[:json] = true
  when '--out' then opts[:out] = ARGV.shift
  when '--out-dir' then opts[:out_dir] = ARGV.shift
  when '--name' then opts[:name] = ARGV.shift
  when '--type' then opts[:type] = ARGV.shift
  else die!("未知参数：#{a}")
  end
end

connect!

case mode
when 'list' then emit(list_state)
when 'ensure-bundle-id' then emit(ensure_bundle_id)
when 'ensure-app' then emit(ensure_app)
when 'create-cert'
  die!('缺少 --type') unless opts[:type]
  emit(create_cert(opts[:type], opts[:out_dir] || File.join(Dir.tmpdir, 'pastejar-certs')))
when 'create-profile'
  die!('缺少 --name') unless opts[:name]
  die!('缺少 --out') unless opts[:out]
  emit(create_profile(opts[:name], File.expand_path(opts[:out])))
else
  die!("未知模式：#{mode.inspect}", '可用：list / ensure-bundle-id / ensure-app / create-cert / create-profile')
end
