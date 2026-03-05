import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Organization } from "../models/components/organization.js";
import { OrganizationCreate } from "../models/components/organizationcreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Organization
 *
 * @remarks
 * Create an organization.
 *
 * **Scopes**: `organizations:write`
 */
export declare function organizationsCreate(client: PolarCore, request: OrganizationCreate, options?: RequestOptions): APIPromise<Result<Organization, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=organizationsCreate.d.ts.map