# MCPort production host

`production.json` creates one ARM64 EC2 host, its own security group and SSM role,
a stable Elastic IP, and a generated-name S3 bucket. Supply an existing VPC/public
subnet and an exact, verified Amazon Linux 2023 ARM64 standard AMI ID for the
selected region. The subnet must belong to the VPC and have working DNS and an
Internet Gateway route. The template does not change that network or other apps.

The default is `t4g.medium` (2 vCPU, 4 GiB RAM) with a 30 GiB encrypted gp3 root
volume. This is initial capacity, not a load-test result. The gateway uses one
SQLite database and must run as a single instance. Data and result growth require
monitoring; this template does not configure retention or automated backups.

## Create and verify

The operator validates and creates the stack using the selected account/region:

```sh
aws cloudformation validate-template --template-body file://deploy/aws/production.json
aws cloudformation create-stack --stack-name silicon-mcport-production \
  --template-body file://deploy/aws/production.json --capabilities CAPABILITY_IAM \
  --enable-termination-protection \
  --parameters ParameterKey=VpcId,ParameterValue=VPC_ID \
    ParameterKey=PublicSubnetId,ParameterValue=SUBNET_ID \
    ParameterKey=AmiId,ParameterValue=EXACT_AL2023_ARM64_AMI
aws cloudformation wait stack-create-complete --stack-name silicon-mcport-production
aws cloudformation describe-stacks --stack-name silicon-mcport-production \
  --query 'Stacks[0].{Protection:EnableTerminationProtection,Outputs:Outputs}'
aws ssm start-session --target INSTANCE_ID
```

No SSH key or inbound port22 is configured. Only TCP80/443 is exposed; the future
gateway remains on `127.0.0.1:4380`. Normal outbound access supports SSM, package
repositories, Silicon Accounts and provider MCP endpoints. IMDSv2 is required with hop limit1.
The initial temporary public address is replaced by the stack's Elastic IP.

CloudFormation completion does not prove cloud-init succeeded. Through SSM,
check `sudo cloud-init status --wait`, `systemctl is-active amazon-ssm-agent`,
`uname -m`, and `python3 --version`. Verify `/var/lib/mcport` is empty and owned by
`mcport:mcport` with mode0700, and `/etc/mcport` is root-owned mode0700.
Bootstrap installs only Python3, libgcc, CA certificates, AWS CLI and SSM plus
these directories/account. It does not fetch secrets or install Caddy/runtime
configuration/the application. There is no application health claim at this stage.

## Permissions and application handoff

The generated instance role has `AmazonSSMManagedInstanceCore` and these scoped
permissions in the selected account/region:

- Read/describe Secrets Manager secrets named `silicon-mcport/production-*` only.
  The stack creates no secret. Use the default Secrets Manager encryption key;
  a customer-managed KMS key needs a separately reviewed decrypt grant/key policy.
- Get exact `releases/*` object keys from this stack's bucket.
- Put/get exact `backups/*` keys. No bucket listing, object deletion, release
  upload or access to other buckets is granted. Use immutable timestamp/revision
  keys; an operator uploads release candidates and manages inventory/retention.

The bucket blocks all public access, disables ACLs, defaults to SSE-S3 encryption,
requires HTTPS and keeps object versions. Upload only reviewed artifacts under
`releases/`; store complete stopped-service database/key/configuration backups
under `backups/`. Do not treat a file copy of a running SQLite database as a backup.
Follow [native installation and recovery](../README.md) after supplying real
runtime credentials, public origins and DNS/TLS. No DNS resources are in this stack.

## Updates, costs and retirement

Enable stack termination protection at creation, and inspect every update's
change set. AMI/subnet changes can replace the instance and move its Elastic IP
to a fresh empty host; this is not an application upgrade or automatic data
migration. Deploy application revisions through the native installer instead.

EC2, public IPv4, EBS, S3 versions/requests and optional Secrets Manager storage
incur charges. The root EBS volume has `DeleteOnTermination: false`; the S3 bucket
has both deletion and replacement retention. They survive instance/stack removal
and continue billing. No automatic object expiry is configured. After validating
recovery and explicitly retiring the service, inventory retained volumes, bucket
versions/delete markers and secrets, then clean them up deliberately. Stack
deletion removes its instance, role and Elastic IP; it does not preserve the IP
or grant the old instance continued access. Termination protection does not
prevent instance replacement during an update.
