using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using NUnit.Framework;

namespace FransHub.FastProof.Tests;

[TestFixture]
public class UnitTests
{
    private static Dictionary<string, string> Env(params (string, string)[] kv) => kv.ToDictionary(x => x.Item1, x => x.Item2);

    [Test]
    public void ConfigIsOffWithoutUrlAndKeepsThePlayground()
    {
        var c = Config.Resolve(null, Env(("FASTPROOF_PROJECT", "shop")), "/tmp");
        Assert.That(c.Enabled, Is.False);
        Assert.That(c.Reason, Does.Contain("FASTPROOF_URL"));
    }

    [Test]
    public void ConfigRejectsABadProjectKey()
    {
        var c = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "Bad Key")), "/tmp");
        Assert.That(c.Enabled, Is.False);
    }

    [Test]
    public void EnabledFalseAlwaysWinsOverOptions()
    {
        var c = Config.Resolve(new FastProofOptions { Url = "https://fastproof.example.com", Project = "shop", Enabled = true },
            Env(("FASTPROOF_ENABLED", "false")), "/tmp");
        Assert.That(c.Enabled, Is.False);
    }

    [Test]
    public void OptionsOverrideTheEnvironment()
    {
        var c = Config.Resolve(new FastProofOptions { Project = "from-options" },
            Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "from-env")), "/tmp");
        Assert.That(c.Project, Is.EqualTo("from-options"));
        Assert.That(c.Enabled, Is.True);
    }

    [Test]
    public void GitLabLaunchIdMatchesTheCli()
    {
        var c = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "shop"), ("GITLAB_CI", "true"),
            ("CI_PROJECT_ID", "42"), ("CI_PIPELINE_ID", "1001"), ("CI_PROJECT_PATH", "group/app"), ("CI_PIPELINE_IID", "7"), ("CI_COMMIT_SHA", "abc")), "/tmp");
        Assert.That(c.ExternalLaunchId, Is.EqualTo("gitlab-42-1001"));
        Assert.That(c.OwnLaunch, Is.False);
        Assert.That(c.Launch["name"]!.GetValue<string>(), Is.EqualTo("group/app #7"));
        Assert.That(c.Launch["ci"]!["buildOrder"]!.GetValue<long>(), Is.EqualTo(7));
        Assert.That(c.Launch["vcs"]!["commit"]!.GetValue<string>(), Is.EqualTo("abc"));
    }

    [Test]
    public void GitHubAndJenkinsLaunchIds()
    {
        var gh = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "shop"), ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "org/app"), ("GITHUB_RUN_ID", "77"), ("GITHUB_RUN_ATTEMPT", "2")), "/tmp");
        Assert.That(gh.ExternalLaunchId, Is.EqualTo("github-org/app-77-2"));
        var jk = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "shop"), ("JENKINS_URL", "https://ci.example.com"),
            ("JOB_NAME", "app"), ("BUILD_NUMBER", "9")), "/tmp");
        Assert.That(jk.ExternalLaunchId, Is.EqualTo("jenkins-app-9"));
    }

    [Test]
    public void WithoutCiTheLaunchIsOwnAndExplicitWins()
    {
        var own = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "shop")), "/tmp");
        Assert.That(own.OwnLaunch, Is.True);
        Assert.That(own.ExternalLaunchId, Does.StartWith("local-"));
        var explicitId = Config.Resolve(null, Env(("FASTPROOF_URL", "https://fastproof.example.com"), ("FASTPROOF_PROJECT", "shop"), ("FASTPROOF_LAUNCH", "mine"), ("GITLAB_CI", "true"), ("CI_PIPELINE_ID", "1")), "/tmp");
        Assert.That(explicitId.ExternalLaunchId, Is.EqualTo("mine"));
        Assert.That(explicitId.OwnLaunch, Is.False);
    }

    [Test]
    public void MaskerMasksEnvironmentValuesTheirEncodedFormAndPatterns()
    {
        var m = new Masker(null, new[] { "literal-secret" }, new[] { @"card-\d{4}" },
            Env(("API_TOKEN", "tok en/9"), ("MY_PASSWORD", "hunter22"), ("SHORT_SECRET", "abc"), ("FLAG_SECRET", "true"), ("PLAIN", "visible-value")));
        Assert.That(m.Apply("a tok en/9 b tok%20en%2F9 c"), Is.EqualTo("a **** b **** c"));
        Assert.That(m.Apply("pw hunter22, literal-secret, card-1234"), Is.EqualTo("pw ****, ****, ****"));
        Assert.That(m.Apply("abc true visible-value"), Is.EqualTo("abc true visible-value"));
    }

    [Test]
    public void TraceContextFormatsAndParses()
    {
        var t = TraceContext.Create("launch 1", "Ns.Test", 2);
        Assert.That(t.Traceparent, Does.Match("^00-[0-9a-f]{32}-[0-9a-f]{16}-01$"));
        Assert.That(TraceContext.Parse(t.Traceparent), Is.EqualTo((t.TraceId, t.SpanId)));
        Assert.That(TraceContext.Parse("00-00000000000000000000000000000000-0000000000000000-01"), Is.Null);
        Assert.That(t.Baggage, Is.EqualTo("fastproof.launch=launch%201,fastproof.test=Ns.Test,fastproof.attempt=2"));
        Assert.That(t.Headers(true)["traceparent"], Is.Not.EqualTo(t.Traceparent));
        Assert.That(t.Env()["TRACEPARENT"], Is.EqualTo(t.Traceparent));
    }

    [Test]
    public void TestPlanMatchesSelectorsAndIds()
    {
        var path = Path.Combine(TestContext.CurrentContext.WorkDirectory, "plan-" + Guid.NewGuid() + ".json");
        File.WriteAllText(path, "{\"version\":\"1.0\",\"tests\":[{\"id\":\"CASE-1\"},{\"selector\":\"Ns.Type.Method\"}]}");
        var plan = TestPlan.Load(path)!;
        var byId = new TestDescriptor();
        byId.PlanIds.Add("CASE-1");
        var bySelector = new TestDescriptor();
        Metadata.AddSelectors(bySelector, "Ns.Type", "Method", "Ns.Type.Method(1)");
        var neither = new TestDescriptor();
        neither.PlanIds.Add("CASE-2");
        Assert.That(plan.Matches(byId), Is.True);
        Assert.That(plan.Matches(bySelector), Is.True);
        Assert.That(plan.Matches(neither), Is.False);
        Assert.That(TestPlan.Load(null), Is.Null);
        File.WriteAllText(path, "not json");
        Assert.Throws(Is.InstanceOf<System.Text.Json.JsonException>(), () => TestPlan.Load(path));
    }

    [Test]
    public void HeaderValuesAreEncodedWhenNotAscii()
    {
        Assert.That(Api.EncodeHeader("shot.png"), Is.EqualTo("shot.png"));
        Assert.That(Api.EncodeHeader("снимок.png"), Does.StartWith("%D1%81"));
    }
}
