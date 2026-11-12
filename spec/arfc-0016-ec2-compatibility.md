# ARFC 0016: EC2 compatibility

- Status: accepted
- Depends on: 0008, 0009, 0010, 0011, 0013
- Crates: alloy-ec2
- Decisions: 0026

## 1. Purpose

Lets existing tools that speak the Amazon EC2 API drive Alloy by changing only
their endpoint and credentials.

## 2. Model

`alloy-ec2` is a stateless translator in front of the native API. It parses EC2
Query requests, verifies Signature Version 4, calls native methods as the
authenticated principal, and renders EC2 XML responses. It holds no state of its
own and can do nothing the native API does not allow.

Mapping:

| EC2 | Alloy |
|---|---|
| account | project. The account id is 12 digits derived from the project uid |
| region, availability zone | region, zone |
| instance, AMI, key pair, instance type | instance, image, key pair, shape |
| VPC, subnet, security group, network interface, route table | the same |
| Elastic IP | public address |
| EBS volume, snapshot | volume, snapshot |
| internet gateway | a route to `public`. Created and attached as an object so tools succeed |
| tags | labels |

Ids are the EC2 prefix and 17 hex digits taken from the resource uid, for example
`i-0a1b2c3d4e5f60718`. The mapping is stable and reversible through an index.

## 3. Behavior

3.1 Endpoint: `ec2.<region>.<cluster-domain>` and `/ec2` on the API endpoint. A
minimal `sts` endpoint answers `GetCallerIdentity`.

3.2 Supported actions, version 1:

- Instances: `RunInstances`, `DescribeInstances`, `DescribeInstanceStatus`,
  `StartInstances`, `StopInstances`, `RebootInstances`, `TerminateInstances`,
  `ModifyInstanceAttribute`, `GetConsoleOutput`, `DescribeInstanceTypes`.
- Images and keys: `DescribeImages`, `CreateImage`, `DeregisterImage`,
  `ImportKeyPair`, `CreateKeyPair`, `DescribeKeyPairs`, `DeleteKeyPair`.
- Network: create, describe, and delete for VPCs, subnets, security groups, route
  tables, routes, internet gateways, and network interfaces, with their attach,
  detach, and associate calls. `AuthorizeSecurityGroupIngress` and `Egress`, and
  the matching `Revoke` calls. `AllocateAddress`, `AssociateAddress`,
  `DisassociateAddress`, `ReleaseAddress`, `DescribeAddresses`.
- Storage: `CreateVolume`, `DescribeVolumes`, `AttachVolume`, `DetachVolume`,
  `ModifyVolume`, `DeleteVolume`, `CreateSnapshot`, `DescribeSnapshots`,
  `DeleteSnapshot`.
- General: `DescribeRegions`, `DescribeAvailabilityZones`, `CreateTags`,
  `DeleteTags`, `DescribeTags`, `DescribeAccountAttributes`.

3.3 Any other action returns the EC2 error `UnsupportedOperation`. A supported
action with an unsupported parameter returns `UnsupportedOperation` naming the
parameter. It must not ignore the parameter.

3.4 Filters, pagination tokens, `DryRun`, client tokens (mapped to idempotency
keys), and error codes follow EC2's documented behavior for the supported actions.

3.5 State names and transitions reported for instances and volumes match EC2's,
mapped from Alloy's.

3.6 Conformance is three pinned suites in `tests/system/ec2`: scripted AWS CLI
sessions, a `boto3` suite, and Terraform configurations using the AWS provider
against Alloy that create, update, and destroy every supported resource with an
empty plan afterwards. The tool versions are pinned in that directory.

3.7 `docs/ec2-compatibility.md` lists every supported action and every known
difference in behavior. It is generated from the same table the translator uses.

## 6. Failure behavior

Native API errors map to the closest EC2 error code, with the native code in the
message. Unknown native errors map to `InternalError`.

## 7. Limits

As the native API. Request size 1 MiB.

## 8. Security

The translator adds no authority: every call is authorized natively as the
signing principal. Upholds I-02, I-15.

## 9. Open questions

- Which further actions the pinned Terraform provider version calls during
  refresh. Research task to be cut in M9.
