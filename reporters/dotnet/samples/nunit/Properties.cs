using FransHub.FastProof.NUnit;
using NUnit.Framework;

[assembly: FastProofReporter]
[assembly: Parallelizable(ParallelScope.Fixtures)]
[assembly: LevelOfParallelism(4)]
