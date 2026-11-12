# Glossary

One term per concept. Use these exact words in specs, code, messages, and docs.

## People and agents

| Term | Meaning |
|---|---|
| pilot | Whoever drives an agent session. Interchangeable, unknown to the repository |
| lead | The orchestrating agent of a session |
| role | One of the defined agent types in `agent/roles/` |
| cloud admin | A person who runs an Alloy cluster: adds nodes, pools, and address ranges |
| org admin | A person who administers one organization inside a cluster |
| user | Anyone or anything that calls the API inside an organization |

## Cluster

| Term | Meaning |
|---|---|
| cluster | One Alloy deployment under one cluster key |
| region | A set of zones sharing one consensus group for regional state |
| zone | A failure domain inside a region, for example a site or a room |
| node | A machine running `alloyd`, joined to a cluster |
| role (node) | What a node does: `anchor`, `control`, `compute`, `storage`. A node may hold several |
| anchor | A node with an address every other node can reach. Reflects addresses, relays traffic, admits joins, and can hold public addresses |
| join token | The string that lets a machine join. Starts with `alyt_` |
| cluster key | The Ed25519 key that signs node records. The root of cluster trust |
| node record | A signed statement binding a node's keys, roles, and addresses |
| seam | A runtime-swappable subsystem boundary: a trait, a registry name, a conformance suite |

## Network

| Term | Meaning |
|---|---|
| underlay | The networks nodes already have: LAN, internet, another overlay |
| mesh | The encrypted overlay between nodes |
| AL | The Alloy Link protocol that carries the mesh. Interfaces are named `al*` |
| session | An AL security association between two nodes |
| path | One way to send AL packets between two nodes: a pair of underlay endpoints, or a relay |
| direct path | A path with no relay, with or without hole punching |
| relayed path | A path through an anchor |
| VPC | A tenant's private layer 3 network |
| VNI | The 24-bit number that identifies a VPC on the mesh |
| ENI | A network interface of an instance inside a VPC |
| public address | An address reachable from outside the cluster, from an address pool |
| holder | The node that attracts traffic for a public address |

## Tenancy

| Term | Meaning |
|---|---|
| organization (org) | The unit of tenancy and isolation |
| project | A grouping of resources inside an org. The unit of quota and default access |
| principal | An identity that can be authorized: user, service account, node, instance role |
| policy | A document of allow and deny statements |

## Compute and storage

| Term | Meaning |
|---|---|
| instance | A virtual machine |
| container | An OCI image run as an isolated workload. In Alloy it runs in its own microVM |
| guest | An instance or a container, when the difference does not matter |
| image | A bootable disk image for instances, or an OCI image for containers |
| pool | A set of disks with one redundancy policy, bound to one storage backend |
| disk | One storage device or directory contributed by a node |
| chunk | The unit of stored data in the chunk store |
| placement group (PG) | A shard of a pool that replicates as one unit |
| volume | A virtual block device |
| bucket, object | As in S3 |
| repository | A named collection of OCI artifacts in the registry |
