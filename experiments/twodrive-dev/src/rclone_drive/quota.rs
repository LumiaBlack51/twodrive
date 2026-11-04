//! Display-only account quota using the already authenticated web session.
//! No password decoding, new sign-in, endpoint override or cookie persistence.
use super::*;
use crate::provider::StorageUsage;

fn account_id(cookies: &str) -> anyhow::Result<String> {
    ensure!(cookies.len() <= 65536, "iCloud 容量会话无效");
    reqwest::header::HeaderValue::from_str(cookies).context("iCloud 容量会话无效")?;
    let user = cookies
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, _)| key.eq_ignore_ascii_case("X-APPLE-WEBAUTH-USER"))
        .map(|(_, value)| value.trim_matches('"'))
        .context("现有 iCloud 会话未提供容量查询信息")?;
    let id = user
        .split(':')
        .find_map(|part| part.strip_prefix("d="))
        .context("现有 iCloud 会话未提供容量查询信息")?;
    ensure!(
        !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit()),
        "iCloud 容量会话无效"
    );
    Ok(id.to_owned())
}
fn parse_usage(value: &Value) -> anyhow::Result<StorageUsage> {
    let usage = &value["storageUsageInfo"];
    let total = usage["totalStorageInBytes"].as_u64();
    let used = usage["usedStorageInBytes"].as_u64();
    ensure!(
        total.is_some() && used.is_some(),
        "iCloud 容量响应缺少有效数值"
    );
    StorageUsage::account(total, used, None).context("iCloud 容量信息暂不可用")
}

pub(super) fn read(
    backend: &RcloneDrive,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<Option<StorageUsage>> {
    check()?;
    let config = {
        let _lock = wait_for_operation(&backend.state, Duration::from_secs(5), &mut *check)?;
        let auth: Auth = backend.state.read(MANIFEST)?;
        ensure!(
            auth.ready && auth.binding == backend.binding,
            "登录状态已改变，拒绝旧连接"
        );
        check()?;
        json_command(
            &backend.exe,
            &backend.state,
            &backend.pass,
            &backend.route,
            &["config", "dump"],
            &backend.cancel,
        )?
    };
    let cookies = config["twodrive"]["cookies"]
        .as_str()
        .context("现有 iCloud 会话未提供容量查询信息")?;
    let id = account_id(cookies)?;
    let mut builder = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15));
    if backend.route == "direct" {
        builder = builder.no_proxy();
    }
    let client = builder.build().context("容量查询网络初始化失败")?;
    check()?;
    let response = client
        .get("https://setup.icloud.com/setup/ws/1/storageUsageInfo")
        .query(&[("dsid", id.as_str())])
        .header(reqwest::header::COOKIE, cookies)
        .header(reqwest::header::ORIGIN, "https://www.icloud.com")
        .header(reqwest::header::REFERER, "https://www.icloud.com/")
        .send()
        .map_err(|_| anyhow::anyhow!("iCloud 容量查询网络暂不可用"))?;
    ensure!(
        response.status().is_success(),
        "iCloud 容量查询暂不可用（HTTP {}），文件连接保持不变",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    response
        .take(131073)
        .read_to_end(&mut bytes)
        .context("容量响应读取失败")?;
    ensure!(bytes.len() <= 131072, "容量响应超过限制");
    check()?;
    let value: Value = serde_json::from_slice(&bytes).context("iCloud 容量响应无效")?;
    Ok(Some(parse_usage(&value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn existing_cookie_identity_and_account_quota_are_bounded_and_strict() {
        assert_eq!(
            account_id("other=ignored; X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=123456\"; token=synthetic")
                .unwrap(),
            "123456"
        );
        for raw in [
            "X-APPLE-WEBAUTH-USER=d=abc",
            "X-APPLE-WEBAUTH-USER=d=1\r\nInjected: x",
            "other=d=123",
            "X-APPLE-WEBAUTH-USER=d=",
        ] {
            assert!(account_id(raw).is_err());
        }
        let quota=parse_usage(&json!({"storageUsageInfo":{"totalStorageInBytes":1000,"usedStorageInBytes":400},"private":"ignored"})).unwrap();
        assert_eq!(quota.free, Some(600));
        let over = parse_usage(
            &json!({"storageUsageInfo":{"totalStorageInBytes":1000,"usedStorageInBytes":1100}}),
        )
        .unwrap();
        assert_eq!(over.free, Some(0));
        assert!(
            parse_usage(
                &json!({"storageUsageInfo":{"totalStorageInBytes":-1,"usedStorageInBytes":0}})
            )
            .is_err()
        );
    }
}
