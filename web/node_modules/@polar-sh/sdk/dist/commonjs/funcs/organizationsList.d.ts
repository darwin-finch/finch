import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { OrganizationsListRequest, OrganizationsListResponse } from "../models/operations/organizationslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Organizations
 *
 * @remarks
 * List organizations.
 *
 * **Scopes**: `organizations:read` `organizations:write`
 */
export declare function organizationsList(client: PolarCore, request: OrganizationsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<OrganizationsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=organizationsList.d.ts.map