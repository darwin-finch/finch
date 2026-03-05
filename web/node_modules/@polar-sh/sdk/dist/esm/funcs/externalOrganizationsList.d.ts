import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { ExternalOrganizationsListRequest, ExternalOrganizationsListResponse } from "../models/operations/externalorganizationslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List External Organizations
 *
 * @remarks
 * List external organizations.
 *
 * **Scopes**: `external_organizations:read`
 */
export declare function externalOrganizationsList(client: PolarCore, request: ExternalOrganizationsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<ExternalOrganizationsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=externalOrganizationsList.d.ts.map