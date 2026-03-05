import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerOrganization } from "../models/components/customerorganization.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalOrganizationsGetRequest } from "../models/operations/customerportalorganizationsget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Organization
 *
 * @remarks
 * Get a customer portal's organization by slug.
 */
export declare function customerPortalOrganizationsGet(client: PolarCore, request: CustomerPortalOrganizationsGetRequest, options?: RequestOptions): APIPromise<Result<CustomerOrganization, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalOrganizationsGet.d.ts.map