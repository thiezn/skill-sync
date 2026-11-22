---
name: cloudformation
description: Coding and configuration standards for AWS CloudFormation templates
---

## Folder structure

Organize your CloudFormation templates and related files in a clear and consistent folder structure. For example:

```
.
├── templates
│   └── template.yml
├── src
│   └── lambda_function_name
│       └── main.py
```

## Parameters

Don't use parameters for everything. Only leverage parameters, and sensible defaults when its likely it will change between deployments to different AWS Accounts.

## Serverless

Leverage the Serverless Application Model (SAM) Framework to efficiently create serverless applications. This helps reduce boilerplate code around AWS Lambda, Application Gateway, API Gateway and others. This includes using the 'CodeUri' property to point to a local directory containing your Lambda function code.
