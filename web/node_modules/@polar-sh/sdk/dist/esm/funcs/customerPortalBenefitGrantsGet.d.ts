import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerBenefitGrant } from "../models/components/customerbenefitgrant.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalBenefitGrantsGetRequest, CustomerPortalBenefitGrantsGetSecurity } from "../models/operations/customerportalbenefitgrantsget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Benefit Grant
 *
 * @remarks
 * Get a benefit grant by ID for the authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalBenefitGrantsGet(client: PolarCore, security: CustomerPortalBenefitGrantsGetSecurity, request: CustomerPortalBenefitGrantsGetRequest, options?: RequestOptions): APIPromise<Result<CustomerBenefitGrant, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalBenefitGrantsGet.d.ts.map